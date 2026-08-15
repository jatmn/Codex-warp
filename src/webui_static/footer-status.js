(() => {
  "use strict";

  // Footer overlay is app-layer policy, not chart-math. chart-math.js 404s in
  // the failure mode this copy describes, so this file is the single source
  // and is prefixed onto app-main.js as /ui/app.js (same response; cannot 404
  // alone). A source map maps the bundle back to these original files.
  const CHARTS_FAILED_STATUS = "Analytics charts failed to load (/ui/chart-math.js)";

  // Chart-math failure is an analytics-tab capability overlay, not a boot hold.
  // remap=false is a held process/boot error: it outranks the overlay.
  // Other tabs keep their own footer. Independent analytics API errors
  // (isError) are appended so a missing module cannot swallow a failed fetch.
  function analyticsDisplayStatus(mathLoaded, tab, proposed, isError, failureText, remap) {
    if (remap === false) {
      return proposed == null ? "" : String(proposed);
    }
    const fail =
      failureText == null || failureText === ""
        ? CHARTS_FAILED_STATUS
        : String(failureText);
    if (mathLoaded || tab !== "analytics") {
      return proposed == null ? "" : String(proposed);
    }
    if (isError && proposed) {
      const extra = String(proposed);
      if (extra && extra !== fail) return `${fail}. ${extra}`;
    }
    return fail;
  }

  const api = {
    chartsFailedStatus: CHARTS_FAILED_STATUS,
    analyticsDisplayStatus,
  };

  const root = typeof globalThis !== "undefined" ? globalThis : this;
  root.CodexWarpFooter = api;
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})();
