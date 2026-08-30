#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const appSource = fs.readFileSync(
  path.join(__dirname, "..", "src", "webui_static", "app-main.js"),
  "utf8",
);

function sourceBetween(start, end) {
  const startAt = appSource.indexOf(start);
  const endAt = appSource.indexOf(end, startAt);
  assert.notEqual(startAt, -1, `missing production source marker: ${start}`);
  assert.notEqual(endAt, -1, `missing production source marker: ${end}`);
  return appSource.slice(startAt, endAt);
}

const visibilitySource = sourceBetween(
  "function analyticsChartVisibility(provider, model) {",
  "function renderAnalyticsPresentation() {",
);
const canvasListSource = sourceBetween(
  "function allChartCanvases() {",
  "function deactivateCharts(",
);

function chartElement(id) {
  const legend = { innerHTML: "stale-legend" };
  const box = {
    hidden: false,
    querySelector(selector) {
      assert.equal(selector, ".chart-legend");
      return legend;
    },
  };
  const canvas = {
    id,
    __chart: { kind: "pie" },
    attrs: null,
    width: 800,
    height: 260,
    cleared: false,
    closest(selector) {
      assert.equal(selector, ".chart-box");
      return box;
    },
    blur() {
      canvas.blurred = true;
    },
    getContext(kind) {
      assert.equal(kind, "2d");
      return {
        clearRect() {
          canvas.cleared = true;
        },
      };
    },
  };
  box.canvas = canvas;
  return { box, canvas, legend };
}

function runtime() {
  const providerPie = chartElement("chart-pie-provider");
  const providerModelsPie = chartElement("chart-pie-provider-models");
  const line = chartElement("chart-line");
  const dismissed = [];
  const attrCalls = [];
  const elements = {
    "#chart-pie-provider": providerPie.canvas,
    "#chart-pie-provider-models": providerModelsPie.canvas,
    "#chart-line": line.canvas,
    "#chart-bar": null,
    "#chart-model-sessions": null,
    "#chart-model-prompts": null,
    "#chart-model-cache-rate": null,
    "#chart-pie-model": null,
  };
  const context = {
    elements,
    dismissed,
    attrCalls,
    synced: 0,
    document: { activeElement: null },
  };
  vm.runInNewContext(
    `
      "use strict";
      const $ = (selector) => globalThis.elements[selector];
      function dismissChartHoverUi(canvas) {
        globalThis.dismissed.push(canvas.id);
      }
      function applyChartCanvasAttrs(canvas, attrs) {
        canvas.attrs = attrs;
        globalThis.attrCalls.push({ id: canvas.id, attrs });
      }
      function legendElFor(canvas) {
        const box = canvas && canvas.closest ? canvas.closest(".chart-box") : null;
        return box ? box.querySelector(".chart-legend") : null;
      }
      function syncChartSurface() {
        globalThis.synced += 1;
      }
      ${visibilitySource}
      ${canvasListSource}
      globalThis.api = {
        analyticsChartVisibility,
        applyAnalyticsChartVisibility,
        allChartCanvases,
        chartCanvases,
      };
    `,
    context,
  );
  return {
    api: context.api,
    providerPie,
    providerModelsPie,
    line,
    dismissed,
    attrCalls,
    getSynced() {
      return context.synced;
    },
  };
}

function check(name, fn) {
  fn();
  process.stdout.write(`ok ${name}\n`);
}

check("provider pie is only active without a provider filter", () => {
  const all = runtime().api.analyticsChartVisibility("", "");
  assert.equal(all.providerPie, true);
  assert.equal(all.perProviderModelPie, false);
  const modelOnly = runtime().api.analyticsChartVisibility("", "model-a");
  assert.equal(modelOnly.providerPie, true);
  assert.equal(modelOnly.perProviderModelPie, false);
});

check("per-provider model pie is only active for a provider with all models", () => {
  const provider = runtime().api.analyticsChartVisibility("openrouter", "");
  assert.equal(provider.providerPie, false);
  assert.equal(provider.perProviderModelPie, true);
  const both = runtime().api.analyticsChartVisibility("openrouter", "model-a");
  assert.equal(both.providerPie, false);
  assert.equal(both.perProviderModelPie, false);
});

check("hides inactive pie boxes instead of leaving blank canvases", () => {
  const run = runtime();
  const allProviders = run.api.applyAnalyticsChartVisibility("", "");
  assert.equal(allProviders.providerPie, true);
  assert.equal(allProviders.perProviderModelPie, false);
  assert.equal(run.providerPie.box.hidden, false);
  assert.equal(run.providerModelsPie.box.hidden, true);
  assert.equal(run.providerModelsPie.canvas.__chart, null);
  assert.equal(run.providerModelsPie.legend.innerHTML, "");
  assert.equal(run.providerModelsPie.canvas.cleared, true);
  assert.deepEqual(run.dismissed, ["chart-pie-provider-models"]);
  assert.equal(run.providerModelsPie.canvas.attrs.ariaHidden, true);
  assert.equal(run.providerModelsPie.canvas.attrs.tabIndex, null);

  const oneProvider = run.api.applyAnalyticsChartVisibility("openrouter", "");
  assert.equal(oneProvider.providerPie, false);
  assert.equal(oneProvider.perProviderModelPie, true);
  assert.equal(run.providerPie.box.hidden, true);
  assert.equal(run.providerModelsPie.box.hidden, false);
  assert.equal(run.providerPie.canvas.__chart, null);

  const bothFilters = run.api.applyAnalyticsChartVisibility("openrouter", "model-a");
  assert.equal(bothFilters.providerPie, false);
  assert.equal(bothFilters.perProviderModelPie, false);
  assert.equal(run.providerPie.box.hidden, true);
  assert.equal(run.providerModelsPie.box.hidden, true);
  assert.ok(run.getSynced() >= 3);
});

check("visible canvas lists omit hidden pie boxes", () => {
  const run = runtime();
  run.api.applyAnalyticsChartVisibility("openrouter", "");
  const visibleIds = Array.from(run.api.chartCanvases(), (canvas) => canvas.id).join(",");
  const allIds = Array.from(run.api.allChartCanvases(), (canvas) => canvas.id).join(",");
  assert.equal(allIds, "chart-line,chart-pie-provider,chart-pie-provider-models");
  assert.equal(visibleIds, "chart-line,chart-pie-provider-models");
});
