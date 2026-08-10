(function (global) {
  "use strict";

  var KEY = "codex-warp-theme";
  var THEME_CHANGE_EVENT = "codex-warp-theme-change";

  function readPreference() {
    try {
      var saved = global.localStorage.getItem(KEY);
      if (saved === "light" || saved === "dark") {
        return saved;
      }
    } catch (e) {
      // Private browsing or blocked storage.
    }
    return global.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  function hasStoredPreference() {
    try {
      var saved = global.localStorage.getItem(KEY);
      return saved === "light" || saved === "dark";
    } catch (e) {
      return false;
    }
  }

  function persistPreference(theme) {
    if (theme !== "light" && theme !== "dark") {
      return;
    }
    try {
      global.localStorage.setItem(KEY, theme);
    } catch (e) {
      // Theme still applies visually; persistence is optional.
    }
  }

  function notifyThemeChange(theme) {
    global.dispatchEvent(
      new CustomEvent(THEME_CHANGE_EVENT, { detail: { theme } }),
    );
  }

  function applyAttribute(theme) {
    if (theme !== "light" && theme !== "dark") {
      return;
    }
    var previous = global.document.documentElement.getAttribute("data-theme");
    global.document.documentElement.setAttribute("data-theme", theme);
    if (previous !== theme) {
      notifyThemeChange(theme);
    }
  }

  function apply(theme, options) {
    var persist = options && options.persist;
    applyAttribute(theme);
    if (persist) {
      persistPreference(theme);
    }
  }

  function getApplied() {
    return global.document.documentElement.getAttribute("data-theme");
  }

  function watchSystemTheme() {
    if (hasStoredPreference()) {
      return;
    }
    var mq = global.matchMedia("(prefers-color-scheme: dark)");
    function onChange() {
      if (hasStoredPreference()) {
        return;
      }
      applyAttribute(readPreference());
    }
    if (mq.addEventListener) {
      mq.addEventListener("change", onChange);
    } else if (mq.addListener) {
      mq.addListener(onChange);
    }
  }

  function installCodexWarpTheme(globalObj) {
    if (globalObj.codexWarpTheme) {
      return globalObj.codexWarpTheme;
    }

    var api = {
      KEY: KEY,
      readPreference: readPreference,
      applyAttribute: applyAttribute,
      persistPreference: persistPreference,
      apply: apply,
      getApplied: getApplied,
    };

    if (!getApplied()) {
      applyAttribute(readPreference());
    }
    watchSystemTheme();

    globalObj.codexWarpTheme = api;
    globalObj.installCodexWarpTheme = installCodexWarpTheme;
    return api;
  }

  installCodexWarpTheme(global);
})(window);
