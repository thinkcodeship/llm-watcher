//! Transient progress output for the fetch phase.
//!
//! Quota endpoints answer in their own time and the timeout is generous, so a
//! run with three accounts can sit silent for a noticeable stretch. Silence and
//! a hang look identical from the outside; this makes the difference visible.
//!
//! Two rules hold everything else together:
//!
//! 1. Every byte goes to **stderr**. `--json` on stdout must stay byte-identical
//!    whether or not a spinner ran, so `llm-watcher --json | jq` cannot break.
//! 2. When stderr is not a terminal the spinner disables itself completely — a
//!    redirected log should not accumulate thousands of cursor-control escapes.
//!
//! The line erases itself when the run finishes, so nothing survives into the
//! scrollback above the table.

use std::io::{IsTerminal, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TICK: Duration = Duration::from_millis(80);

/// `\r` returns to column zero, `\x1b[2K` erases the line it sits on. Only ever
/// written to a terminal, so the escape is always understood.
const CLEAR: &str = "\r\x1b[2K";

/// The braille frame for a tick, cycling forever.
pub fn frame(tick: usize) -> char {
    FRAMES[tick % FRAMES.len()]
}

/// One rendered status line: `⠹ [2/3] querying minimax-max`.
///
/// `current` is 1-based and clamped to `total`, because an off-by-one in the
/// caller should not produce a visible `[4/3]`.
pub fn render(tick: usize, current: usize, total: usize, label: &str) -> String {
    format!(
        "{} [{}/{}] querying {label}",
        frame(tick),
        current.min(total),
        total
    )
}

/// A single self-erasing status line driven by a background thread.
///
/// Constructing one with [`Spinner::new`] starts drawing; dropping it (or
/// calling [`Spinner::finish`]) stops and erases. A disabled spinner is inert
/// but keeps the same API, so callers never branch on whether output is a tty.
pub struct Spinner {
    state: Arc<Mutex<(usize, String)>>,
    /// Dropping the sender disconnects the channel, which wakes the draw thread
    /// immediately instead of leaving it to finish its current sleep.
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    /// A spinner drawing to stderr, or an inert one when stderr is not a tty.
    pub fn new(total: usize) -> Self {
        if !std::io::stderr().is_terminal() {
            return Self::disabled();
        }

        let state = Arc::new(Mutex::new((0usize, String::new())));
        let (tx, rx) = mpsc::channel::<()>();
        let thread_state = Arc::clone(&state);

        let handle = thread::spawn(move || {
            let mut tick = 0usize;
            loop {
                // The lock is released before writing: an I/O stall must never
                // block the main thread's next `set`.
                let (current, label) = {
                    let guard = thread_state.lock().unwrap_or_else(|e| e.into_inner());
                    (guard.0, guard.1.clone())
                };
                // Nothing is drawn until the first `set`, so a run that fails
                // before any fetch leaves the terminal untouched.
                if !label.is_empty() {
                    let mut err = std::io::stderr().lock();
                    let _ = write!(err, "{CLEAR}{}", render(tick, current, total, &label));
                    let _ = err.flush();
                }
                tick = tick.wrapping_add(1);

                match rx.recv_timeout(TICK) {
                    Err(RecvTimeoutError::Timeout) => continue,
                    _ => break,
                }
            }
        });

        Self {
            state,
            stop: Some(tx),
            handle: Some(handle),
        }
    }

    /// An inert spinner: every method is a no-op and nothing is ever written.
    pub fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new((0, String::new()))),
            stop: None,
            handle: None,
        }
    }

    /// Name the account now in flight. `current` is 1-based.
    pub fn set(&self, current: usize, label: &str) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *guard = (current, label.to_string());
    }

    /// Stop drawing and erase the line. Idempotent, so [`Drop`] can call it
    /// again after an explicit call without erasing a line someone else drew.
    pub fn finish(&mut self) {
        let was_drawing = self.stop.take().is_some();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if was_drawing {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "{CLEAR}");
            let _ = err.flush();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_names_the_account_and_its_position() {
        assert_eq!(
            render(0, 2, 3, "minimax-max"),
            "⠋ [2/3] querying minimax-max"
        );
    }

    #[test]
    fn frames_cycle_and_wrap_without_panicking() {
        assert_eq!(frame(0), '⠋');
        assert_eq!(frame(FRAMES.len()), '⠋', "the cycle must wrap to the start");
        assert_eq!(frame(FRAMES.len() * 3 + 2), FRAMES[2]);
        // A long-running fetch ticks a lot; indexing must never go out of range.
        assert_eq!(frame(usize::MAX), FRAMES[usize::MAX % FRAMES.len()]);
    }

    #[test]
    fn the_frame_is_the_only_thing_that_changes_between_ticks() {
        let a = render(0, 1, 2, "glm");
        let b = render(1, 1, 2, "glm");
        assert_ne!(a, b, "the spinner must visibly animate");
        assert_eq!(
            a[a.char_indices().nth(1).unwrap().0..],
            b[b.char_indices().nth(1).unwrap().0..]
        );
    }

    #[test]
    fn an_overshooting_counter_is_clamped_rather_than_shown() {
        assert_eq!(render(0, 9, 3, "x"), "⠋ [3/3] querying x");
    }

    #[test]
    fn a_zero_account_run_renders_without_dividing_by_anything() {
        assert_eq!(render(0, 0, 0, "x"), "⠋ [0/0] querying x");
    }

    #[test]
    fn the_status_line_carries_no_color_escapes() {
        // Colour would survive into a NO_COLOR run; the line is deliberately plain.
        assert!(!render(3, 1, 1, "glm").contains('\x1b'));
    }

    #[test]
    fn a_disabled_spinner_is_inert_and_safe_to_drive() {
        let mut spinner = Spinner::disabled();
        spinner.set(1, "minimax");
        spinner.set(2, "glm");
        spinner.finish();
        spinner.finish(); // idempotent
    }

    #[test]
    fn finishing_twice_does_not_double_erase() {
        // Under `cargo test` stderr is captured, so this exercises the disabled
        // path; the point is that the state machine survives the sequence.
        let mut spinner = Spinner::new(2);
        spinner.set(1, "minimax");
        spinner.finish();
        spinner.finish();
    }

    #[test]
    fn a_spinner_dropped_without_finishing_cleans_itself_up() {
        let spinner = Spinner::new(1);
        spinner.set(1, "minimax");
        drop(spinner); // must join the thread rather than leak or hang
    }

    #[test]
    fn set_is_safe_to_call_from_another_thread() {
        let spinner = Arc::new(Spinner::disabled());
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let spinner = Arc::clone(&spinner);
                thread::spawn(move || spinner.set(i, "concurrent"))
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
