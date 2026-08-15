#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const path = require("node:path");
const charts = require(path.join(__dirname, "..", "src/webui_static/chart-math.js"));
const footer = require(path.join(__dirname, "..", "src/webui_static/footer-status.js"));

function check(name, fn) {
  fn();
  process.stdout.write(`ok ${name}\n`);
}

check("integerTicks stays on integer breaks", () => {
  const { ticks, top } = charts.integerTicks(3);
  assert.ok(ticks.every((t) => Number.isInteger(t)));
  assert.equal(ticks[0], 0);
  assert.ok(top >= 3);
  assert.equal(ticks[ticks.length - 1], top);
});

check("integerTicks rejects non-finite max", () => {
  const { ticks, top } = charts.integerTicks(Infinity);
  assert.deepEqual(ticks, [0, 1]);
  assert.equal(top, 1);
});

check("bucketLabelStyle follows timestamp collisions, not range names", () => {
  const day1 = Date.UTC(2026, 7, 14, 10, 0);
  const day1b = Date.UTC(2026, 7, 14, 11, 0);
  const day2 = Date.UTC(2026, 7, 15, 10, 0);
  assert.equal(charts.bucketLabelStyle([{ ts: day1 }, { ts: day1b }]), "time");
  assert.equal(charts.formatBucketLabel(day1, "time"), "10:00");
  // 24h is hourly across two UTC days: same clock time must not reuse HH:MM.
  const hourlyTwoDays = [{ ts: day1 }, { ts: day1b }, { ts: day2 }];
  assert.equal(charts.bucketLabelStyle(hourlyTwoDays), "datetime");
  assert.notEqual(
    charts.formatBucketLabel(day1, "datetime"),
    charts.formatBucketLabel(day2, "datetime"),
  );
  const daily = [
    { ts: Date.UTC(2026, 7, 14, 0, 0) },
    { ts: Date.UTC(2026, 7, 15, 0, 0) },
    { ts: Date.UTC(2026, 7, 16, 0, 0) },
  ];
  assert.equal(charts.bucketLabelStyle(daily), "date");
  assert.equal(charts.formatBucketLabel(day1, "date"), "8/14");
  // Distinct clock times on two UTC days still need the date (today @ midnight).
  const acrossMidnight = [
    { ts: Date.UTC(2026, 7, 14, 23, 0) },
    { ts: Date.UTC(2026, 7, 15, 0, 0) },
  ];
  assert.equal(charts.bucketLabelStyle(acrossMidnight), "datetime");
});

check("fitCanvasMetrics keeps drawing in CSS pixels on HiDPI", () => {
  const m = charts.fitCanvasMetrics(400, 0, 800, 2, 220);
  assert.equal(m.cssW, 400);
  assert.equal(m.cssH, 220);
  assert.equal(m.bufferW, 800);
  assert.equal(m.bufferH, 440);
  assert.equal(10 * (m.cssW / m.bufferW) * 2, 10);
});

check("canvasCssWidth never treats the HiDPI buffer as CSS width", () => {
  assert.equal(charts.canvasCssWidth(360, 800, 800), 360);
  assert.equal(charts.canvasCssWidth(0, 360, 800), 360);
  assert.equal(charts.canvasCssWidth(0, 0, 800), 800);
  assert.notEqual(charts.canvasCssWidth(0, 0, 800), 1600);
});

check("layoutChartPlot never inverts the plot on a narrow canvas", () => {
  const layout = charts.layoutChartPlot(100, 220, { padL: 46, padR: 88, padT: 30, padB: 26 });
  assert.ok(layout.plotW >= 1);
  assert.equal(layout.padL + layout.padR + layout.plotW, 100);
  assert.ok(layout.plotH >= 1);
});

check("layoutLegendChips keeps every series within two rows", () => {
  const measure = (text) => String(text).length * 6;
  const items = [
    ["Total tokens", "t"],
    ["Cached tokens", "c"],
    ["Prompts", "p"],
    ["Sessions", "s"],
  ];
  const style = charts.legendChipChrome();
  const rowWidth = (row, gap = 6) =>
    row.reduce((sum, chip, i) => sum + chip.width + (i ? gap : 0), 0);
  const assertPaintable = (chip) => {
    assert.ok(chip.width + 1e-6 >= style.minChip);
    assert.equal(chip.pad, style.pad);
    assert.equal(chip.swatch, style.swatch);
    assert.equal(chip.labelX, style.labelX);
    if (chip.label) {
      assert.ok(chip.width + 1e-6 >= chip.labelX + measure(chip.label));
    }
  };
  const wide = charts.layoutLegendChips(items, measure, 800, { maxRows: 2 });
  assert.equal(wide.rows.length, 1);
  assert.equal(wide.overflow, false);
  assert.equal(wide.rows[0].length, 4);
  assert.equal(wide.rows[0][1].label, "Cached tokens");
  assert.ok(rowWidth(wide.rows[0]) <= 800);
  wide.rows.flat().forEach(assertPaintable);
  const narrow = charts.layoutLegendChips(items, measure, 80, { maxRows: 2 });
  assert.equal(narrow.overflow, false);
  assert.ok(narrow.rows.length >= 1);
  assert.ok(narrow.rows.length <= 2);
  assert.equal(narrow.rows.flat().length, 4);
  assert.equal(narrow.rows.flat().map((chip) => chip.color).join(""), "tcps");
  for (const row of narrow.rows) assert.ok(rowWidth(row) <= 80 + 1e-6);
  narrow.rows.flat().forEach(assertPaintable);
  const tiny = charts.layoutLegendChips(items, measure, 20, { maxRows: 2 });
  assert.equal(tiny.overflow, true);
  assert.ok(tiny.rows.length <= 2);
  assert.equal(tiny.rows.flat().length, 4);
  tiny.rows.flat().forEach(assertPaintable);
  const clip = charts.legendPaintClip(70, 20, 54);
  assert.equal(clip.x, 70);
  assert.equal(clip.width, 20);
  assert.equal(clip.height, 54);
  for (const row of tiny.rows) {
    assert.ok(rowWidth(row) > clip.width);
  }
  const emptyBudget = charts.layoutLegendChips(items, measure, 0, { maxRows: 2 });
  assert.equal(emptyBudget.overflow, true);
  assert.equal(emptyBudget.rows.flat().length, 4);
  emptyBudget.rows.flat().forEach(assertPaintable);
  const uneven = (text) => (String(text).includes("W") ? 90 : 6);
  const mixed = charts.layoutLegendChips(
    [
      ["WW", "w"],
      ["ii", "i"],
    ],
    uneven,
    40,
    { maxRows: 1 },
  );
  assert.ok(mixed.rows[0][0].label.length < 2 || mixed.rows[0][0].label.includes("…"));
  const measure4 = (text) => String(text).length * 6;
  const four = [
    ["Total tokens", "t"],
    ["Cached tokens", "c"],
    ["Prompts", "p"],
    ["Sessions", "s"],
  ];
  const three = [
    ["Total tokens", "t"],
    ["Prompts", "p"],
    ["Sessions", "s"],
  ];
  const assertReadable = (layout) => {
    for (const chip of layout.rows.flat()) {
      assert.ok(chip.label && String(chip.label).trim(), "legend chip label must stay readable");
    }
  };
  const at240 = charts.layoutLegendChips(four, measure4, 88, { maxRows: 2, gap: 6 });
  assert.equal(at240.overflow, false);
  assertReadable(at240);
  assert.equal(charts.legendSecondRowPad(at240), at240.rows.length > 1 ? 24 : 0);
  const at352 = charts.layoutLegendChips(four, measure4, 200, { maxRows: 2, gap: 6 });
  assert.equal(at352.overflow, false);
  assertReadable(at352);
  assert.equal(at352.rows.length, 2);
  assert.deepEqual(
    at352.rows.flat().map((chip) => chip.label),
    ["Total tokens", "Cached tokens", "Prompts", "Sessions"],
  );
  assert.equal(charts.legendSecondRowPad(at352), 24);
  const mid = charts.layoutLegendChips(four, measure4, 162, { maxRows: 2, gap: 6 });
  assert.equal(mid.overflow, false);
  assert.equal(mid.rows.length, 2);
  assertReadable(mid);
  const midChars = mid.rows.flat().reduce((sum, chip) => sum + String(chip.label).length, 0);
  const oneRow = charts.layoutLegendChips(four, measure4, 162, { maxRows: 1, gap: 6 });
  const oneChars = oneRow.rows.flat().reduce((sum, chip) => sum + String(chip.label).length, 0);
  assert.ok(midChars > oneChars, "two rows must beat a one-row ellipsis pack");
  const noCached = charts.layoutLegendChips(three, measure4, 108, { maxRows: 2, gap: 6 });
  assert.equal(noCached.overflow, false);
  assert.equal(noCached.rows.flat().length, 3);
  assertReadable(noCached);
  const overflow = charts.layoutLegendChips(four, measure4, 20, { maxRows: 2, gap: 6 });
  assert.equal(overflow.overflow, true);
  assert.ok(overflow.rows.length > 1);
  const overflowPad = charts.legendSecondRowPad(overflow);
  assert.equal(overflowPad, 24);
  const overflowClip = charts.legendPaintClip(70, 20, 30 + overflowPad);
  const lastRowBottom = charts.legendChipRowY(overflow.rows.length - 1) + 16;
  assert.ok(lastRowBottom <= overflowClip.height, "overflow second row must stay inside padT clip");
});

check("barSlotLayout keeps a drawable slot when gaps would overflow", () => {
  const dense = charts.barSlotLayout(138, 60);
  assert.ok(dense.slot > 0);
  assert.ok(dense.barW > 0);
  assert.ok(dense.barW + dense.barGap <= dense.slot + 1e-9);
  const wide = charts.barSlotLayout(700, 24);
  assert.ok(wide.barGap <= 6);
  assert.ok(wide.barW > 1);
});

check("pointerCssX maps client X into CSS chart coordinates", () => {
  assert.equal(charts.pointerCssX(150, 100, 200, 400), 100);
  assert.equal(charts.pointerCssX(100, 100, 0, 400), 0);
});

check("reconcileHoverTs drops identity when the bucket disappears", () => {
  const full = [{ ts: 1 }, { ts: 2 }, { ts: 3 }];
  assert.equal(charts.reconcileHoverTs(full, 3), 3);
  assert.equal(charts.reconcileHoverTs(full.slice(0, 2), 3), null);
  assert.equal(charts.reconcileHoverTs([], 3), null);
});

check("chartFocusAction takes keyboard ownership of the current bucket", () => {
  assert.deepEqual(charts.chartFocusAction(2, 5), {
    kind: "keyboard",
    idx: 2,
    inputMode: "keyboard",
    clearMouse: true,
  });
  assert.deepEqual(charts.chartFocusAction(-1, 5), {
    kind: "keyboard",
    idx: 4,
    inputMode: "keyboard",
    clearMouse: true,
  });
  assert.equal(charts.chartFocusAction(-1, 0).kind, "noop");
});

check("resolveIdxByTs drops hover when the series shrinks past the bucket", () => {
  const full = [{ ts: 1 }, { ts: 2 }, { ts: 3 }];
  assert.equal(charts.resolveIdxByTs(full, 3), 2);
  assert.equal(charts.resolveIdxByTs(full.slice(0, 2), 3), -1);
  assert.equal(charts.resolveIdxByTs([], 3), -1);
});

check("nextKeyboardIdx walks from the last bucket when none is selected", () => {
  assert.equal(charts.nextKeyboardIdx(-1, 5, 1), 4);
  assert.equal(charts.nextKeyboardIdx(4, 5, -1), 3);
  assert.equal(charts.nextKeyboardIdx(0, 5, -1), 0);
  assert.equal(charts.nextKeyboardIdx(0, 0, 1), -1);
});

check("barSlotLayout paints a visible bar when the slot is subpixel", () => {
  const tiny = charts.barSlotLayout(40, 80);
  assert.ok(tiny.slot > 0);
  assert.ok(tiny.barW > 0);
  assert.ok(tiny.barW <= tiny.slot + 1e-9);
});

check("barPaintRect grows subpixel heights so positive values paint", () => {
  const tiny = charts.barPaintRect(500, 100000, 164);
  assert.ok(tiny.barH >= 1);
  assert.equal(tiny.y + tiny.barH, 164);
  const zero = charts.barPaintRect(0, 100000, 164);
  assert.equal(zero.barH, 0);
  const tall = charts.barPaintRect(100000, 100000, 164);
  assert.equal(tall.barH, 164);
  assert.equal(tall.y, 0);
});

check("barAnchorY uses the painted bar top, not the linear axis mapping", () => {
  const padT = 30;
  const plotH = 164;
  const top = 100000;
  const painted = charts.barPaintRect(500, top, plotH);
  assert.equal(charts.barAnchorY(500, top, plotH, padT), padT + painted.y);
  const linear = padT + (1 - 500 / top) * plotH;
  assert.notEqual(charts.barAnchorY(500, top, plotH, padT), linear);
  assert.equal(charts.barAnchorY(0, top, plotH, padT), padT + plotH);
});

check("tokenAxisAnchorTokens follows the higher painted token marker", () => {
  assert.equal(charts.tokenAxisAnchorTokens(100, 40, true), 100);
  assert.equal(charts.tokenAxisAnchorTokens(100, 250, true), 250);
  assert.equal(charts.tokenAxisAnchorTokens(100, 250, false), 100);
  assert.equal(charts.tokenAxisAnchorTokens(100, 0, true), 100);
});

check("tooltipFollowsPointer is pointer-owned only when mouse coords exist", () => {
  assert.equal(charts.tooltipFollowsPointer("pointer", { x: 1, y: 2 }), true);
  assert.equal(charts.tooltipFollowsPointer("pointer", null), false);
  assert.equal(charts.tooltipFollowsPointer("keyboard", { x: 1, y: 2 }), false);
});

check("nearestIdxByX ignores points outside the hover threshold", () => {
  assert.equal(charts.nearestIdxByX([10, 50, 90], 52, 14), 1);
  assert.equal(charts.nearestIdxByX([10, 50, 90], 30, 14), -1);
  assert.equal(charts.nearestIdxByX([], 10, 14), -1);
});

check("barIndexAtX uses the slot, not the painted bar width", () => {
  assert.equal(charts.barIndexAtX(0, 10, 5), 0);
  assert.equal(charts.barIndexAtX(19.9, 10, 5), 1);
  assert.equal(charts.barIndexAtX(-1, 10, 5), -1);
  assert.equal(charts.barIndexAtX(50, 10, 5), -1);
});

check("announceIfChanged skips identical poll redraws", () => {
  const first = charts.announceIfChanged("", "10:00: Total tokens 1");
  assert.equal(first.changed, true);
  const poll = charts.announceIfChanged(first.text, first.text);
  assert.equal(poll.changed, false);
  const next = charts.announceIfChanged(first.text, "10:01: Total tokens 2");
  assert.equal(next.changed, true);
});

check("chartInputStep: Tab onto a pointer hover becomes keyboard-owned", () => {
  const points = [{ ts: 1 }, { ts: 2 }, { ts: 3 }];
  const hovered = charts.chartInputStep(
    { points, hoverTs: 2, inputMode: "pointer", hasMouse: true },
    { type: "focus" },
  );
  assert.equal(hovered.inputMode, "keyboard");
  assert.equal(hovered.hasMouse, false);
  assert.equal(hovered.hoverTs, 2);
  const keyed = charts.chartInputStep(hovered, { type: "keydown", key: "ArrowRight" });
  assert.equal(keyed.preventDefault, true);
  assert.equal(keyed.hoverTs, 3);
  assert.equal(keyed.inputMode, "keyboard");
  const leaveWhileKeys = charts.chartInputStep(keyed, { type: "mouseleave" });
  assert.equal(leaveWhileKeys.hoverTs, 3);
  assert.equal(leaveWhileKeys.inputMode, "keyboard");
  const miss = charts.chartInputStep(leaveWhileKeys, { type: "mousemove", hitTs: null });
  assert.equal(miss.inputMode, "keyboard");
  assert.equal(miss.hoverTs, 3);
  assert.equal(miss.hasMouse, false);
  assert.equal(miss.claimExclusive, false);
  const pointerBack = charts.chartInputStep(leaveWhileKeys, { type: "mousemove", hitTs: 1 });
  assert.equal(pointerBack.inputMode, "pointer");
  assert.equal(pointerBack.hoverTs, 1);
  assert.equal(pointerBack.hasMouse, true);
  assert.equal(pointerBack.claimExclusive, true);
});

check("chartInputStep: pointer miss clears pointer hover but not keyboard", () => {
  const points = [{ ts: 1 }, { ts: 2 }];
  const pointerMiss = charts.chartInputStep(
    { points, hoverTs: 2, inputMode: "pointer", hasMouse: true },
    { type: "mousemove", hitTs: null },
  );
  assert.equal(pointerMiss.inputMode, "pointer");
  assert.equal(pointerMiss.hoverTs, null);
  assert.equal(pointerMiss.hasMouse, true);
});

check("liveRegionText is empty when no bucket is selected", () => {
  assert.equal(charts.liveRegionText(-1, "10:00: Total tokens 1"), "");
  assert.equal(charts.liveRegionText(0, "10:00: Total tokens 1"), "10:00: Total tokens 1");
  const stay = charts.announceIfChanged("10:00: Total tokens 1", charts.liveRegionText(-1, "10:00: Total tokens 1"));
  assert.equal(stay.changed, true);
  assert.equal(stay.text, "");
});

check("chartsLiveLayout is current CSS width, not lastCssW", () => {
  assert.equal(charts.chartsLiveLayout(400), true);
  assert.equal(charts.chartsLiveLayout(0), false);
});

check("shouldPaintCharts paints only with a measured CSS width", () => {
  assert.equal(charts.shouldPaintCharts(400, 0), true);
  assert.equal(charts.shouldPaintCharts(0, 0), false);
  assert.equal(charts.shouldPaintCharts(0, 360), true);
});

check("chartSurface is idle until math, buckets, and live layout exist", () => {
  assert.equal(charts.chartSurface(false, 8, true), "failed");
  assert.equal(charts.chartSurface(true, 0, true), "idle");
  assert.equal(charts.chartSurface(true, 3, false), "idle");
  assert.equal(charts.chartSurface(true, 3, true), "interactive");
  assert.equal(charts.chartSurface(true, 3, charts.chartsLiveLayout(0)), "idle");
});

check("analyticsDisplayStatus remaps only the analytics tab when math is missing", () => {
  const fail = footer.chartsFailedStatus;
  assert.equal(fail, "Analytics charts failed to load (/ui/chart-math.js)");
  assert.equal(
    footer.analyticsDisplayStatus(false, "analytics", "Analytics updated", false, fail),
    fail,
  );
  assert.equal(
    footer.analyticsDisplayStatus(false, "analytics", "Loading…", false),
    fail,
  );
  assert.equal(
    footer.analyticsDisplayStatus(false, "providers", "Ready", false, fail),
    "Ready",
  );
  assert.equal(
    footer.analyticsDisplayStatus(true, "analytics", "Analytics updated", false, fail),
    "Analytics updated",
  );
  assert.equal(
    footer.analyticsDisplayStatus(false, "analytics", "Analytics error: boom", true, fail),
    `${fail}. Analytics error: boom`,
  );
  assert.equal(
    footer.analyticsDisplayStatus(false, "analytics", "Error: boom", true),
    `${fail}. Error: boom`,
  );
  assert.equal(
    footer.analyticsDisplayStatus(true, "analytics", "Analytics error: boom", true, fail),
    "Analytics error: boom",
  );
  assert.equal(
    footer.analyticsDisplayStatus(false, "analytics", "Error: providers failed", true, fail, false),
    "Error: providers failed",
  );
  assert.equal(
    footer.analyticsDisplayStatus(true, "analytics", "Error: providers failed", true, fail, false),
    "Error: providers failed",
  );
});

check("chartCanvasAttrs revokes the full AT surface when disabled", () => {
  const on = charts.chartCanvasAttrs("interactive");
  assert.equal(on.tabIndex, 0);
  assert.equal(on.role, "application");
  assert.equal(on.keyshortcuts, "ArrowLeft ArrowRight");
  assert.equal(on.describedBy, "chart-kbd-help");
  assert.equal(on.labelledBy, true);
  assert.equal(on.ariaHidden, null);
  assert.equal(on.kbdHelpHidden, false);
  assert.equal(on.fallbackHidden, true);
  const idle = charts.chartCanvasAttrs("idle");
  assert.equal(idle.tabIndex, null);
  assert.equal(idle.role, null);
  assert.equal(idle.keyshortcuts, null);
  assert.equal(idle.labelledBy, true);
  assert.equal(idle.ariaHidden, null);
  assert.equal(idle.kbdHelpHidden, true);
  assert.equal(idle.fallbackHidden, true);
  const off = charts.chartCanvasAttrs("failed");
  assert.equal(off.tabIndex, null);
  assert.equal(off.role, null);
  assert.equal(off.keyshortcuts, null);
  assert.equal(off.describedBy, null);
  assert.equal(off.labelledBy, null);
  assert.equal(off.ariaHidden, true);
  assert.equal(off.kbdHelpHidden, true);
  assert.equal(off.fallbackHidden, false);
  assert.deepEqual(charts.chartCanvasAttrs(true), on);
  assert.deepEqual(charts.chartCanvasAttrs(false), off);
});

check("chartInputStep deactivate drops pointer ownership like blur", () => {
  const points = [{ ts: 1 }, { ts: 2 }];
  const hovered = charts.chartInputStep(
    { points, hoverTs: 2, inputMode: "pointer", hasMouse: true },
    { type: "deactivate" },
  );
  assert.equal(hovered.hoverTs, null);
  assert.equal(hovered.hasMouse, false);
  assert.equal(hovered.inputMode, "pointer");
  const keyed = charts.chartInputStep(
    { points, hoverTs: 1, inputMode: "keyboard", hasMouse: false },
    { type: "deactivate" },
  );
  assert.equal(keyed.hoverTs, null);
  assert.equal(keyed.inputMode, "pointer");
});

check("pieSlices starts at 12 o'clock and sweeps clockwise", () => {
  const { slices, total } = charts.pieSlices([2, 1, 1]);
  assert.equal(total, 4);
  assert.equal(slices.length, 3);
  assert.equal(slices[0].start, -Math.PI / 2);
  assert.equal(slices[0].end, slices[0].start + Math.PI);
  assert.equal(slices[1].start, slices[0].end);
  assert.equal(slices[1].end, slices[1].start + Math.PI / 2);
  assert.equal(slices[2].end, slices[2].start + Math.PI / 2);
  assert.equal(slices[2].end, -Math.PI / 2 + Math.PI * 2);
});

check("pieSlices gives zero-width arcs to zero values", () => {
  const { slices, total } = charts.pieSlices([0, 5, 0]);
  assert.equal(total, 5);
  assert.equal(slices[0].start, slices[0].end);
  assert.equal(slices[1].end - slices[1].start, Math.PI * 2);
  assert.equal(slices[2].start, slices[2].end);
});

check("pieSlices handles empty and all-zero input", () => {
  const empty = charts.pieSlices([]);
  assert.equal(empty.total, 0);
  assert.deepEqual(empty.slices, []);
  const zeros = charts.pieSlices([0, 0]);
  assert.equal(zeros.total, 0);
  assert.equal(zeros.slices[0].start, zeros.slices[0].end);
  assert.equal(zeros.slices[1].start, zeros.slices[1].end);
});

check("pieSliceIndexAt hits the correct slice and rejects misses", () => {
  const { slices } = charts.pieSlices([1, 1, 1, 1]);
  const cx = 100;
  const cy = 100;
  const r = 50;
  // 3 o'clock (angle 0) is the second quarter: start at -PI/2 + PI/2 = 0.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx + r, cy), 1);
  // 6 o'clock (angle PI/2) is the third quarter.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx, cy + r), 2);
  // 9 o'clock (angle PI) is the fourth quarter.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx - r, cy), 3);
  // 12 o'clock (angle -PI/2) is the first quarter.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx, cy - r), 0);
  // Outside the radius is a miss.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx + r * 2, cy), -1);
  // Inside a donut hole is a miss.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 30, slices, cx + 10, cy), -1);
  // The exact center of a full pie is ambiguous (atan2(0, 0) has no defined
  // slice), so it is treated as a miss instead of arbitrarily selecting a
  // slice.
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx, cy), -1);
});

check("pieSliceIndexAt skips zero-width slices", () => {
  const { slices } = charts.pieSlices([5, 0, 5]);
  const cx = 100;
  const cy = 100;
  const r = 50;
  // The zero slice consumed no angle: the first half still owns angle 0
  // (3 o'clock) and the second half owns angle PI/2 (6 o'clock).
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx + r, cy), 0);
  assert.equal(charts.pieSliceIndexAt(cx, cy, r, 0, slices, cx, cy + r), 2);
});

check("pieSlices clamps non-finite and negative values to zero", () => {
  const { slices, total } = charts.pieSlices([5, Infinity, NaN, -3, 2]);
  assert.equal(total, 7);
  assert.equal(slices.length, 5);
  // Only the finite positive values consume angle.
  assert.equal(slices[1].end - slices[1].start, 0);
  assert.equal(slices[2].end - slices[2].start, 0);
  assert.equal(slices[3].end - slices[3].start, 0);
  assert.ok(Number.isFinite(slices[4].end));
});

check("pieMidAngle is the slice center for labels", () => {
  const { slices } = charts.pieSlices([1, 1]);
  assert.equal(charts.pieMidAngle(slices[0]), -Math.PI / 2 + Math.PI / 2);
  assert.equal(charts.pieMidAngle(null), 0);
});

check("textColorOn picks dark text on light fills with WCAG luminance", () => {
  // Amber-600: white text was 3.19:1 (below AA); dark text is ~5:1.
  assert.equal(charts.textColorOn("#d97706"), "#1f2937");
  // Teal-700: white text on this fill passes comfortably.
  assert.equal(charts.textColorOn("#0f766e"), "#ffffff");
  // Invalid/missing colors default to white text.
  assert.equal(charts.textColorOn(""), "#ffffff");
  assert.equal(charts.textColorOn("nope"), "#ffffff");
});

check("reconcilePieHover keeps identity and drops removed keys", () => {
  const rows = [
    { key: "a", value: 5 },
    { key: "b", value: 3 },
  ];
  assert.equal(charts.reconcilePieHover(rows, "b"), "b");
  assert.equal(charts.reconcilePieHover(rows, "missing"), null);
  assert.equal(charts.reconcilePieHover(rows, null), null);
});

check("reconcilePieHover drops a key whose value collapsed to zero", () => {
  const rows = [
    { key: "a", value: 100 },
    { key: "b", value: 0 },
  ];
  assert.equal(charts.reconcilePieHover(rows, "a"), "a");
  // A zero-value row has no visible slice; keeping its hover would leave a
  // phantom ring/tooltip on an invisible wedge after a poll redraw.
  assert.equal(charts.reconcilePieHover(rows, "b"), null);
  assert.equal(charts.reconcilePieHover(rows, "a"), "a");
});

process.stdout.write("webui chart harness: all checks passed\n");
