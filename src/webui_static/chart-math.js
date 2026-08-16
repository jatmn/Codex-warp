(() => {
  "use strict";

  // Integer "nice" ticks for count/token data. Counts are whole numbers, so the
  // step is forced to an integer and the axis max rounds up to a clean multiple
  // of the step. This keeps axis labels meaningful (never fractional tokens or
  // prompts) regardless of how small or large the data max is.
  function integerTicks(max, target) {
    const want = Math.max(1, target == null ? 4 : target);
    const value = Math.max(1, Number(max) || 0);
    if (!Number.isFinite(value)) {
      return { ticks: [0, 1], top: 1 };
    }
    const raw = value / want;
    const mag = 10 ** Math.floor(Math.log10(raw));
    let step = mag;
    for (const m of [1, 2, 2.5, 5, 10]) {
      if (raw <= m * mag) {
        step = m * mag;
        break;
      }
    }
    step = Math.max(1, Math.ceil(step));
    const top = Math.ceil(value / step) * step;
    const ticks = [];
    for (let v = 0; v <= value; v += step) ticks.push(v);
    if (ticks[ticks.length - 1] !== top) ticks.push(top);
    return { ticks, top };
  }

  function utcDatePart(ms) {
    const d = new Date(ms);
    return `${d.getUTCMonth() + 1}/${d.getUTCDate()}`;
  }

  function utcTimePart(ms) {
    const d = new Date(ms);
    return `${String(d.getUTCHours()).padStart(2, "0")}:${String(
      d.getUTCMinutes(),
    ).padStart(2, "0")}`;
  }

  // Choose a label from the actual timestamps. Unique HH:MM values are not
  // "same day" — a series that crosses midnight still needs the date even
  // when 23:00 and 00:00 do not collide as clock times.
  function bucketLabelStyle(points) {
    if (!points || !points.length) return "time";
    const dates = points.map((p) => utcDatePart(p.ts));
    const times = points.map((p) => utcTimePart(p.ts));
    const uniqueDates = new Set(dates);
    const uniqueTimes = new Set(times);
    if (uniqueDates.size <= 1) return "time";
    if (uniqueDates.size === dates.length && uniqueTimes.size === 1) return "date";
    return "datetime";
  }

  function formatBucketLabel(ms, style) {
    if (!ms) return "";
    const date = utcDatePart(ms);
    const time = utcTimePart(ms);
    if (style === "date") return date;
    if (style === "datetime") return `${date} ${time}`;
    return time;
  }

  function canvasCssWidth(clientWidth, lastCssW, fallback) {
    if (clientWidth > 0) return clientWidth;
    if (lastCssW > 0) return lastCssW;
    return fallback > 0 ? fallback : 800;
  }

  // Buffer is CSS size × DPR. Drawing must use cssW/cssH after setTransform(dpr),
  // never the buffer pixel size, or HiDPI text shrinks to 10/dpr CSS pixels.
  function fitCanvasMetrics(clientWidth, lastCssW, fallback, dpr, cssHeight) {
    const cssW = canvasCssWidth(clientWidth, lastCssW, fallback);
    const cssH = cssHeight || 220;
    const ratio = dpr > 0 ? dpr : 1;
    return {
      cssW,
      cssH,
      bufferW: Math.max(1, Math.round(cssW * ratio)),
      bufferH: Math.max(1, Math.round(cssH * ratio)),
    };
  }

  // Keep a positive plot area when the canvas is narrower than the preferred
  // padding. Padding yields to the plot, never the other way around.
  function layoutChartPlot(width, height, preferred) {
    const w = Math.max(1, Number(width) || 0);
    const h = Math.max(1, Number(height) || 0);
    const wantL = preferred && preferred.padL != null ? preferred.padL : 46;
    const wantR = preferred && preferred.padR != null ? preferred.padR : 16;
    const wantT = preferred && preferred.padT != null ? preferred.padT : 30;
    const wantB = preferred && preferred.padB != null ? preferred.padB : 26;
    const minPlot = preferred && preferred.minPlot != null ? preferred.minPlot : 40;
    let padL = wantL;
    let padR = wantR;
    let padT = wantT;
    let padB = wantB;
    if (w - padL - padR < minPlot) {
      const budget = Math.max(0, w - minPlot);
      const totalPad = padL + padR;
      if (totalPad <= 0) {
        padL = 0;
        padR = 0;
      } else {
        padL = Math.floor((budget * padL) / totalPad);
        padR = budget - padL;
      }
    }
    if (h - padT - padB < 20) {
      const budget = Math.max(0, h - 20);
      const totalPad = padT + padB;
      if (totalPad <= 0) {
        padT = 0;
        padB = 0;
      } else {
        padT = Math.floor((budget * padT) / totalPad);
        padB = budget - padT;
      }
    }
    return {
      padL,
      padR,
      padT,
      padB,
      plotW: Math.max(1, w - padL - padR),
      plotH: Math.max(1, h - padT - padB),
    };
  }

  // Pack legend chips into a bounded number of rows in the top padding band.
  // Layout and paint share this chrome: pad + swatch + label gap + right pad.
  // Never shrink a chip below the swatch box; ellipsize labels down to a
  // single character instead of blanking them. Overflow is allowed only when
  // even two rows of min-width chips cannot fit the budget.
  function legendChipChrome(options) {
    const pad = options && options.pad != null ? Number(options.pad) : 4;
    const swatch = options && options.swatch != null ? Number(options.swatch) : 8;
    const labelGap = options && options.labelGap != null ? Number(options.labelGap) : 4;
    const rightPad = options && options.rightPad != null ? Number(options.rightPad) : 6;
    const labelX = pad + swatch + labelGap;
    return {
      pad,
      swatch,
      labelGap,
      rightPad,
      labelX,
      chrome: labelX + rightPad,
      minChip: pad + swatch + pad,
    };
  }

  // Vertical pitch of legend rows in the reserved top band. Paint and padT
  // extra must share this so a packed second row cannot sit on the clip edge.
  const LEGEND_ROW_PITCH = 24;
  const LEGEND_ROW_Y0 = 6;

  function legendChipRowY(rowIndex) {
    return LEGEND_ROW_Y0 + (Number(rowIndex) || 0) * LEGEND_ROW_PITCH;
  }

  // Extra top padding follows packed row count, not horizontal overflow.
  // Overflow still paints every packed row (clip handles width); withholding
  // padT would clip those rows vertically.
  function legendSecondRowPad(layout) {
    const rows = layout && layout.rows;
    const count = rows && rows.length ? rows.length : 0;
    if (count <= 1) return 0;
    return (count - 1) * LEGEND_ROW_PITCH;
  }

  function layoutLegendChips(items, measureText, rowBudget, options) {
    const gap = options && options.gap != null ? Number(options.gap) : 6;
    const style = legendChipChrome(options);
    const maxRows = Math.max(1, options && options.maxRows != null ? Number(options.maxRows) : 2);
    const budget = Number(rowBudget);
    const list = (items || []).map((item) => {
      if (Array.isArray(item)) return { label: String(item[0] || ""), color: item[1] };
      return { label: String(item && item.label ? item.label : ""), color: item && item.color };
    });
    const measure = (text) => {
      if (typeof measureText !== "function") return String(text || "").length;
      const width = Number(measureText(text == null ? "" : String(text)));
      return Number.isFinite(width) && width > 0 ? width : 0;
    };
    const finiteBudget = Number.isFinite(budget) ? budget : 0;
    const chipWidth = (label) => {
      const text = label || "";
      if (!text) return style.minChip;
      return Math.max(style.minChip, style.chrome + measure(text));
    };
    const rowPixelWidth = (labels) =>
      labels.reduce((sum, label, i) => sum + chipWidth(label) + (i ? gap : 0), 0);

    function partitionSizes(n, rowCount) {
      if (rowCount <= 0 || n <= 0) return [];
      if (rowCount === 1) return [[n]];
      if (rowCount > n) return [];
      const out = [];
      for (let first = 1; first <= n - rowCount + 1; first++) {
        for (const rest of partitionSizes(n - first, rowCount - 1)) {
          out.push([first, ...rest]);
        }
      }
      return out;
    }

    function groupsBySizes(source, sizes) {
      const groups = [];
      let start = 0;
      for (const size of sizes) {
        groups.push(source.slice(start, start + size));
        start += size;
      }
      return groups;
    }

    function ellipsize(labels) {
      const out = labels.map((label) => String(label || ""));
      let guard = 0;
      while (rowPixelWidth(out) > finiteBudget && guard++ < 400) {
        let idx = -1;
        let widest = -1;
        for (let i = 0; i < out.length; i++) {
          const base = out[i].endsWith("…") ? out[i].slice(0, -1) : out[i];
          if (!base || base.length <= 1) continue;
          const width = chipWidth(out[i]);
          if (width > widest) {
            widest = width;
            idx = i;
          }
        }
        if (idx < 0) break;
        const current = out[idx];
        const base = current.endsWith("…") ? current.slice(0, -1) : current;
        out[idx] = `${base.slice(0, -1)}…`;
      }
      return out;
    }

    function fitLabels(labels, allowOverflow) {
      const out = ellipsize(labels);
      const total = rowPixelWidth(out);
      if (!allowOverflow && total > finiteBudget) return null;
      return out.map((label) => ({ label, width: chipWidth(label) }));
    }

    function toChips(group, fitted) {
      return fitted.map((chip, i) => ({
        label: chip.label,
        color: group[i].color,
        width: chip.width,
        pad: style.pad,
        swatch: style.swatch,
        labelX: style.labelX,
      }));
    }

    function scorePacked(packed) {
      const labels = packed.flat().map((chip) => chip.label || "");
      const readable = labels.filter((label) => String(label).trim()).length;
      const chars = labels.reduce((sum, label) => sum + String(label).length, 0);
      return readable * 1000 + chars;
    }

    function pack(rowCount, allowOverflow) {
      const sizeOptions =
        rowCount === 1 ? [[list.length]] : partitionSizes(list.length, rowCount);
      let best = null;
      let bestScore = -1;
      for (const sizes of sizeOptions) {
        const groups = groupsBySizes(list, sizes);
        const packed = [];
        for (const group of groups) {
          const fitted = fitLabels(
            group.map((item) => item.label),
            allowOverflow,
          );
          if (!fitted) {
            packed.length = 0;
            break;
          }
          packed.push(toChips(group, fitted));
        }
        if (!packed.length) continue;
        const score = scorePacked(packed);
        if (score > bestScore) {
          bestScore = score;
          best = packed;
        }
      }
      return best;
    }

    if (!list.length) return { rows: [], style, overflow: false };
    const capped = Math.min(maxRows, list.length);
    // Score every non-overflow row count. First-fit prefers a 1-row pack that
    // "fits" only after shrinking labels to one character, even when two rows
    // would keep the series names readable. Tie-break toward fewer rows so a
    // wide canvas stays on one line.
    let best = null;
    let bestScore = -1;
    let bestRowCount = Infinity;
    for (let rowCount = 1; rowCount <= capped; rowCount++) {
      const packed = pack(rowCount, false);
      if (!packed) continue;
      const score = scorePacked(packed);
      if (score > bestScore || (score === bestScore && rowCount < bestRowCount)) {
        best = packed;
        bestScore = score;
        bestRowCount = rowCount;
      }
    }
    if (best) return { rows: best, style, overflow: false };
    return { rows: pack(capped, true) || [], style, overflow: true };
  }

  // Legend paint is clipped to the reserved top band. Packing may still emit
  // min-width swatches when the budget cannot hold them; clip keeps those
  // boxes off the plot and the right-axis column.
  function legendPaintClip(startX, budget, height) {
    return {
      x: Number(startX) || 0,
      y: 0,
      width: Math.max(0, Number(budget) || 0),
      height: Math.max(0, Number(height) || 0),
    };
  }

  // Gap is a remainder of each slot, never an independent cost that can consume
  // the whole plot and leave barW at 0. Hit-testing should use `slot`, not barW.
  function barSlotLayout(plotW, n) {
    const count = Math.max(1, n | 0);
    const width = Math.max(0, Number(plotW) || 0);
    const slot = width / count;
    const desiredGap = count > 60 ? 1 : 6;
    const gap = count > 1 ? Math.min(desiredGap, Math.max(0, slot * 0.35)) : 0;
    let barW = Math.max(0, slot - gap);
    // Canvas fillRect drops subpixel widths. Grow into the slot (up to 1px)
    // so a positive slot always paints and remains hittable.
    if (barW > 0 && barW < 1) barW = Math.min(1, slot);
    return { barW, barGap: gap, slot };
  }

  // Same contract as bar width: fillRect drops subpixel heights, so a positive
  // token count must grow to 1 CSS pixel and sit on the plot baseline.
  function barPaintRect(val, top, plotH) {
    const value = Number(val) || 0;
    const max = top > 0 ? top : 0;
    const height = plotH > 0 ? plotH : 0;
    if (!(value > 0) || !(max > 0) || !(height > 0)) {
      return { barH: 0, y: height };
    }
    let barH = (value / max) * height;
    if (barH > 0 && barH < 1) barH = Math.min(1, height);
    return { barH, y: height - barH };
  }

  // Keyboard/pointer-anchor Y must use the painted bar top, not the linear
  // axis mapping. Subpixel values sit on a 1px bar at the baseline.
  function barAnchorY(val, top, plotH, padT) {
    return (Number(padT) || 0) + barPaintRect(val, top, plotH).y;
  }

  // Keyboard/non-pointer line-chart tooltips sit on the highest token-axis
  // marker for the bucket. Cached can exceed total (cache reads reported
  // outside input tokens), so the anchor must follow the painted rings.
  function tokenAxisAnchorTokens(total, cached, hasCachedData) {
    const totalTokens = Number(total) || 0;
    if (!hasCachedData) return totalTokens;
    const cachedTokens = Number(cached) || 0;
    return cachedTokens > 0 ? Math.max(totalTokens, cachedTokens) : totalTokens;
  }

  // Pointer tooltips follow the cursor. Keyboard (and pointer-without-coords)
  // must anchor to the selected bucket, or leftover mouse coords win.
  function tooltipFollowsPointer(inputMode, hasMouse) {
    return inputMode === "pointer" && !!hasMouse;
  }

  function nearestIdxByX(xs, mx, threshold) {
    if (!xs || !xs.length) return -1;
    const limit = threshold == null ? 14 : threshold;
    let best = -1;
    let bestDist = Infinity;
    for (let i = 0; i < xs.length; i++) {
      const dist = Math.abs(xs[i] - mx);
      if (dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    }
    if (best < 0 || bestDist > limit) return -1;
    return best;
  }

  function barIndexAtX(rel, slot, n) {
    if (!(rel >= 0) || !(slot > 0) || !(n > 0)) return -1;
    const idx = Math.floor(rel / slot);
    if (idx < 0 || idx >= n) return -1;
    return idx;
  }

  function pointerCssX(clientX, rectLeft, rectWidth, cssW) {
    if (!rectWidth) return 0;
    return ((clientX - rectLeft) * cssW) / rectWidth;
  }

  function resolveIdxByTs(points, hoverTs) {
    if (hoverTs == null || !points || !points.length) return -1;
    return points.findIndex((p) => p.ts === hoverTs);
  }

  function reconcileHoverTs(points, hoverTs) {
    return resolveIdxByTs(points, hoverTs) < 0 ? null : hoverTs;
  }

  function nextKeyboardIdx(cur, len, delta) {
    if (!len) return -1;
    const start = cur < 0 ? len - 1 : cur;
    return Math.max(0, Math.min(len - 1, start + delta));
  }

  // Focus is keyboard ownership. Preserve an existing bucket, but leftover
  // mouse coordinates are not pointer ownership — Tab onto a hovered chart
  // must still get a bucket-anchored tooltip. Pointer reclaim is a hit, not
  // mere mousemove over padding or empty plot.
  function chartFocusAction(resolvedIdx, len) {
    if (!len) return { kind: "noop", idx: -1, inputMode: null, clearMouse: false };
    const idx = resolvedIdx >= 0 ? resolvedIdx : nextKeyboardIdx(-1, len, 0);
    return { kind: "keyboard", idx, inputMode: "keyboard", clearMouse: true };
  }

  // Chart interaction policy, independent of DOM listeners. Tests this
  // instead of hoping attachChartHover stays in sync with comments.
  function chartInputStep(state, event) {
    const points = (state && state.points) || [];
    const len = points.length;
    const hoverTs = state && state.hoverTs;
    const resolved = resolveIdxByTs(points, hoverTs);
    const inputMode = (state && state.inputMode) || "pointer";
    const hasMouse = !!(state && state.hasMouse);
    const base = { points, hoverTs, inputMode, hasMouse, preventDefault: false };

    if (!event || !event.type) return base;
    if (event.type === "mousemove") {
      const hitTs = Object.prototype.hasOwnProperty.call(event, "hitTs")
        ? event.hitTs
        : null;
      if (hitTs != null) {
        return {
          ...base,
          inputMode: "pointer",
          hasMouse: true,
          hoverTs: hitTs,
          claimExclusive: true,
        };
      }
      // A miss must not steal keyboard ownership. Pointer mode still tracks
      // the cursor so leaving the plot can clear a pointer hover.
      if (inputMode === "keyboard") {
        return { ...base, hasMouse: false, claimExclusive: false };
      }
      return {
        ...base,
        inputMode: "pointer",
        hasMouse: true,
        hoverTs: null,
        claimExclusive: false,
      };
    }
    if (event.type === "mouseleave") {
      if (inputMode === "keyboard") return { ...base, hasMouse: false };
      return { ...base, hasMouse: false, hoverTs: null, inputMode: "pointer" };
    }
    if (event.type === "focus") {
      const action = chartFocusAction(resolved, len);
      if (action.kind === "noop") return base;
      return {
        ...base,
        inputMode: "keyboard",
        hasMouse: false,
        hoverTs: points[action.idx].ts,
        claimExclusive: true,
      };
    }
    if (event.type === "blur" || event.type === "deactivate") {
      return { ...base, hasMouse: false, hoverTs: null, inputMode: "pointer" };
    }
    if (event.type === "keydown") {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return base;
      if (!len) return base;
      const delta = event.key === "ArrowRight" ? 1 : -1;
      const next = nextKeyboardIdx(resolved, len, delta);
      return {
        ...base,
        inputMode: "keyboard",
        hasMouse: false,
        hoverTs: points[next].ts,
        preventDefault: true,
      };
    }
    return base;
  }

  function announceIfChanged(previous, next) {
    const prev = previous == null ? "" : String(previous);
    const value = next == null ? "" : String(next);
    if (prev === value) return { text: value, changed: false };
    return { text: value, changed: true };
  }

  // Live text is a function of the selected bucket. No selection (pointer left,
  // series shrink, empty data, tab hide) must announce empty — not "keep the
  // last tooltip until blur".
  function liveRegionText(selectedIdx, summary) {
    if (!(selectedIdx >= 0)) return "";
    return summary == null ? "" : String(summary);
  }

  // Live layout is current CSS width. Hidden panels report 0; do not treat a
  // remembered buffer as "the panel is visible" (polls would paint off-tab).
  function chartsLiveLayout(clientWidth) {
    return Number(clientWidth) > 0;
  }

  // Hidden panels report clientWidth 0. Inventing an 800px CSS width there
  // poisons hover math. A previous real layout (`lastCssW`) is safe to reuse
  // for deactivate redraws; skip only when no measured width exists.
  function shouldPaintCharts(clientWidth, lastCssW) {
    return chartsLiveLayout(clientWidth) || Number(lastCssW) > 0;
  }

  // Three surfaces, not a boolean. HTML must ship idle: keyboard application
  // semantics exist only after math loaded, there is at least one bucket, and
  // the canvas is live-laid-out. "failed" is a missing module (show fallback).
  // "idle" is a working chart with nothing to navigate, or buckets that have
  // not been laid out yet.
  function chartSurface(mathLoaded, bucketCount, liveLaidOut) {
    if (!mathLoaded) return "failed";
    if (Number(bucketCount) > 0 && liveLaidOut) return "interactive";
    return "idle";
  }

  function chartCanvasAttrs(surface) {
    const kind =
      surface === true || surface === "interactive"
        ? "interactive"
        : surface === "idle"
          ? "idle"
          : "failed";
    if (kind === "interactive") {
      return {
        tabIndex: 0,
        role: "application",
        keyshortcuts: "ArrowLeft ArrowRight",
        describedBy: "chart-kbd-help",
        labelledBy: true,
        ariaHidden: null,
        kbdHelpHidden: false,
        fallbackHidden: true,
      };
    }
    if (kind === "idle") {
      return {
        tabIndex: null,
        role: null,
        keyshortcuts: null,
        describedBy: null,
        labelledBy: true,
        ariaHidden: null,
        kbdHelpHidden: true,
        fallbackHidden: true,
      };
    }
    return {
      tabIndex: null,
      role: null,
      keyshortcuts: null,
      describedBy: null,
      labelledBy: null,
      ariaHidden: true,
      kbdHelpHidden: true,
      fallbackHidden: false,
    };
  }

  const api = {
    integerTicks,
    bucketLabelStyle,
    formatBucketLabel,
    canvasCssWidth,
    fitCanvasMetrics,
    layoutChartPlot,
    legendChipChrome,
    legendChipRowY,
    legendSecondRowPad,
    layoutLegendChips,
    legendPaintClip,
    barSlotLayout,
    barPaintRect,
    barAnchorY,
    tokenAxisAnchorTokens,
    tooltipFollowsPointer,
    nearestIdxByX,
    barIndexAtX,
    pointerCssX,
    resolveIdxByTs,
    reconcileHoverTs,
    nextKeyboardIdx,
    chartFocusAction,
    chartInputStep,
    announceIfChanged,
    liveRegionText,
    chartsLiveLayout,
    shouldPaintCharts,
    chartSurface,
    chartCanvasAttrs,
  };

  const root = typeof globalThis !== "undefined" ? globalThis : this;
  root.CodexWarpCharts = api;
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})();
