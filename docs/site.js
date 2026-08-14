/* llm-watcher project page — progressive enhancement only.
   Every branch here is optional: with this file blocked, absent or failing,
   the page still renders complete, readable and navigable. */

(function () {
  "use strict";

  var root = document.documentElement;

  /* ── theme ───────────────────────────────────────────────────────── */

  var toggle = document.querySelector("[data-theme-toggle]");

  function systemPrefersDark() {
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  }

  function readStored() {
    try { return localStorage.getItem("llm-watcher-theme"); } catch (e) { return null; }
  }

  function applyTheme(theme) {
    if (theme) {
      root.setAttribute("data-theme", theme);
    } else {
      root.removeAttribute("data-theme");
    }
    if (!toggle) return;
    var dark = theme ? theme === "dark" : systemPrefersDark();
    toggle.setAttribute("aria-pressed", String(dark));
    var label = toggle.querySelector(".theme-toggle__label");
    // The button names the mode it switches *to*.
    if (label) label.textContent = dark ? "Light" : "Dark";
  }

  applyTheme(readStored());

  if (toggle) {
    toggle.addEventListener("click", function () {
      var nowDark = root.getAttribute("data-theme")
        ? root.getAttribute("data-theme") === "dark"
        : systemPrefersDark();
      var next = nowDark ? "light" : "dark";
      applyTheme(next);
      try { localStorage.setItem("llm-watcher-theme", next); } catch (e) { /* private mode */ }
    });
  }

  /* ── copy buttons ────────────────────────────────────────────────── */

  var status = document.querySelector("[data-copy-status]");

  Array.prototype.forEach.call(document.querySelectorAll("[data-copy]"), function (btn) {
    btn.addEventListener("click", function () {
      var text = btn.getAttribute("data-copy");
      var done = function (ok) {
        btn.textContent = ok ? "Copied" : "Select it";
        if (ok) btn.setAttribute("data-copied", "");
        if (status) status.textContent = ok ? "Command copied to clipboard" : "Copy unavailable — select the command manually";
        setTimeout(function () {
          btn.textContent = "Copy";
          btn.removeAttribute("data-copied");
        }, 2000);
      };

      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(false); });
      } else {
        done(false);
      }
    });
  });

  /* No scroll reveal lives here on purpose. The hero entrance is a pure
     CSS animation, so nothing on this page depends on this file to become
     visible — the worst a failure here can do is leave the theme on the
     system default and the copy buttons inert. */
})();
