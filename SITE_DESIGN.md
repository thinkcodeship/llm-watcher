# Site design — llm-watcher project page

The specification behind `docs/`, the GitHub Pages site. Written down so the
next change to the page is a decision rather than a guess.

## The problem the page solves

Someone arrives from a search result, a README badge, or a link in a thread.
They have thirty seconds and one question: *what is this, and is it for me?*
The page has to answer that before they scroll, then hand them an install
command. It is not a documentation site — the README is canonical, and
duplicating it here guarantees the two drift apart. Everything deep links out.

## Concept — 間 (ma)

The tool measures intervals. A rolling five-hour window, a weekly window, the
span until each rolls over. `ma` is the Japanese reading of that same idea in
design: the interval between things, treated as material rather than leftover.

So the page is built out of its gaps. Sections are separated by space, not by
boxes, borders, or cards. Rules are hairlines. There is one accent colour and it
is spent sparingly, the way a seal is pressed once on a finished page.

This is not decoration chosen to look Japanese. It matches what the program
already is: runs, prints, exits. A page for a tool that refuses to run a daemon
should not ship a carousel.

The one thing to remember: **the seal**. A vermilion square stamped with 監視
(*kanshi* — to watch), set against a page that is otherwise nearly empty. It is
the mark, the favicon, and the only saturated colour above the fold.

## Palette

Sumi ink on washi, one vermilion. Both themes are first-class; the dark theme is
not an inversion but a separate mixture, warmer and lower in contrast than a
pure black would be.

| Token | Light | Dark | Use |
|-------|-------|------|-----|
| `--paper` | `#faf8f5` | `#16150f` | Page ground |
| `--paper-sunk` | `#f2eee7` | `#1e1d16` | Terminal block, inset panels |
| `--ink` | `#1a1a1a` | `#eae6dd` | Body text |
| `--ink-soft` | `#57534a` | `#a29c8f` | Secondary text, captions |
| `--ink-faint` | `#726b60` | `#8b8478` | Labels, captions, annotation |
| `--rule` | `#ddd7cc` | `#33312a` | Hairlines |
| `--vermilion` | `#b7410e` | `#d95f2b` | Seal, links, single accent |

Three semantic colours carry the pace reading, and they are the same three the
CLI prints, because the page is teaching someone to read terminal output:

| Token | Light | Dark | Meaning |
|-------|-------|------|---------|
| `--pace-ok` | `#3f6f43` | `#7fae72` | below `1.0x` |
| `--pace-warn` | `#8a5d0c` | `#d0a13c` | `1.0x` to `1.5x` |
| `--pace-over` | `#a3301f` | `#e07a63` | `1.5x` and above |

Colour never carries meaning alone. Every pace state is also stated in words, so
the reading survives a monochrome screen and colour-blind vision alike.

## Typography

Two families, self-hosted, subset. No third-party request is made by this page —
consistent with a tool that opens no port and writes nothing back.

- **Zen Old Mincho** — display and body prose. A genuine mincho with real stroke
  modulation; carries the 監視 glyphs of the seal in the same voice as the Latin.
  Weights 400 and 700.
- **IBM Plex Mono** — terminal output, code, data, labels, and the eyebrow text.
  Weights 400 and 500. Chosen partly because it carries the box-drawing glyphs
  (`└ ─ │ ├`) the real output uses, so the sample block needs no fallback font
  and cannot lose its alignment.

Subsets are Latin plus the punctuation actually used, plus 監視間 for the mincho
and the box-drawing range for the mono: 38 KB for four files. `font-display:
swap`, with a metric-tolerant fallback stack behind each.

Scale is a fourth-based ramp on a 16px root, fluid via `clamp()` between 380px
and 1100px viewports. Body copy sits at 17px with `1.75` leading and a measure
capped at `62ch` — long enough to read, short enough that the margin stays part
of the composition.

## Layout

A single column, `min(68rem, 100% - 2*gutter)`, deliberately narrow. Content sits
left; the right margin is left open rather than filled, which is where most of
the `ma` lives. Section rhythm is `clamp(5rem, 11vh, 9rem)` of vertical space,
with a hairline only where a topic genuinely ends.

Sections, in order:

1. **Hero** — seal, wordmark, the one-line question the tool answers, the real
   terminal output, and the install command.
2. **Reading a row** — the anatomy diagram. The single most useful thing on the
   page: one row of real output with its three parts annotated. Connector lines
   are drawn in CSS, not typed as ASCII, so they stay crisp and reflow on
   narrow screens into a stacked list.
3. **Pace** — the formula, then the three states as labelled bars showing quota
   consumed against clock elapsed. This is the concept the tool is built on.
4. **What it does not do** — no daemon, no database, no port, no writes to
   `~/.claude/`. Set as a list of absences, which is both the honest selling
   point and the clearest expression of the concept.
5. **Install & configure** — `cargo install`, then the multi-account
   `config.toml` that is the reason the tool exists.
6. **Providers** — the three endpoints, with the `Bearer`-prefix asymmetry
   called out, since getting it wrong produces a `401` that reads like a bad key.
7. **Footer** — Kanshiro LLC, set at display size with the seal, linking
   thinkcodeship.com. MIT, © 2026, repository link.

## Motion

Restraint is the rule. One orchestrated entrance: hero elements fade and rise
8px on a 60ms stagger. Section content reveals once on scroll via
`IntersectionObserver`. Hairlines draw from the left over 600ms. Pace bars fill
when scrolled into view.

Everything is `transform`/`opacity` only, all of it inside
`@media (prefers-reduced-motion: no-preference)`. With reduced motion requested
the page renders complete and static — the reveal never gates content, so a
failed observer or a blocked script leaves nothing hidden.

## Accessibility

- Contrast meets WCAG AA (4.5:1) for normal text at every ink-on-ground pairing
  in the tables above, in both themes and against both grounds — measured, not
  assumed. `--ink-faint` and light `--pace-warn` were darkened from their first
  values specifically to clear that bar.
- Semantic landmarks, one `h1`, ordered headings, skip link to `main`.
- The terminal sample is real selectable text in a `<figure>` with a caption,
  not an image, so it can be read aloud and copied.
- Copy buttons announce their result via `aria-live`; the command stays
  selectable if the Clipboard API is unavailable or denied.
- Focus is always visible: a 2px vermilion ring with offset, never removed.
- Decorative glyphs carry `aria-hidden`; the seal has a real label.

## Deployment contract

Static files under `docs/`, served by GitHub Pages set to **main branch,
`/docs` folder**. No build step, no CI job, no Jekyll — `.nojekyll` is committed
to skip it. Assets are referenced relatively so the project-page base path
(`/llm-watcher/`) resolves without configuration.

The consequence to respect: `docs/` is published the moment it lands on `main`.
Nothing belongs in it that is not meant to be public.
