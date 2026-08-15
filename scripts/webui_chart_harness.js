#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const path = require("node:path");
const charts = require(path.join(__dirname, "..", "src/webui_static/chart-math.js"));

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

check("shouldPaintCharts refuses hidden or unlaid-out canvases", () => {
  assert.equal(charts.shouldPaintCharts(true, 400), true);
  assert.equal(charts.shouldPaintCharts(false, 400), false);
  assert.equal(charts.shouldPaintCharts(true, 0), false);
  assert.equal(charts.shouldPaintCharts(false, 0), false);
});

check("chartSurface is idle until math loaded and buckets exist", () => {
  assert.equal(charts.chartSurface(false, 8), "failed");
  assert.equal(charts.chartSurface(true, 0), "idle");
  assert.equal(charts.chartSurface(true, 3), "interactive");
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

process.stdout.write("webui chart harness: all checks passed\n");
