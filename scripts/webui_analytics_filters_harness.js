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

const readHelperSource = sourceBetween(
  "function readStoredAnalyticsFilters()",
  "let managementToken =",
);
const restoreInitializerSource = sourceBetween(
  "let analyticsFiltersToRestore = readStoredAnalyticsFilters();",
  "let managementTokenPrompt =",
);
const filterHelperSource = sourceBetween(
  "function analyticsOptionValue(select, saved)",
  "let analyticsPending =",
);

function select(value, values) {
  return {
    value,
    options: values.map((optionValue) => ({ value: optionValue })),
  };
}

function storage(initial = null) {
  return {
    value: initial,
    throwGet: false,
    throwSet: false,
    getItem(key) {
      assert.equal(key, "codex-warp-webui-analytics-filters");
      if (this.throwGet) throw new Error("storage read blocked");
      return this.value;
    },
    setItem(key, value) {
      assert.equal(key, "codex-warp-webui-analytics-filters");
      if (this.throwSet) throw new Error("storage write blocked");
      this.value = String(value);
    },
  };
}

function runtime({ stored = null, range = "24h", provider = "", model = "" } = {}) {
  const sessionStorage = storage(stored);
  const elements = {
    "#analytics-range": select(range, ["1h", "24h", "week"]),
    "#analytics-provider": select(provider, ["", "configured"]),
    "#analytics-model": select(model, [""]),
  };
  const context = {
    elements,
    fillHook: () => {},
    sessionStorage,
  };
  vm.runInNewContext(
    `
      "use strict";
      const ANALYTICS_FILTERS_KEY = "codex-warp-webui-analytics-filters";
      const $ = (selector) => globalThis.elements[selector];
      function fillAnalyticsFilters() { globalThis.fillHook(); }
      ${readHelperSource}
      ${restoreInitializerSource}
      ${filterHelperSource}
      globalThis.filters = {
        readStoredAnalyticsFilters,
        writeAnalyticsFilters,
        storeAnalyticsFilters,
        restoreAnalyticsFilters,
        settleAnalyticsInventoryChange,
        setPending(value) { analyticsFiltersToRestore = value; },
        getPending() { return analyticsFiltersToRestore; },
      };
    `,
    context,
  );
  return {
    elements,
    filters: context.filters,
    sessionStorage,
    setFillHook(hook) { context.fillHook = hook; },
  };
}

function parsedStorage(run) {
  return JSON.parse(run.sessionStorage.value);
}

function check(name, fn) {
  fn();
  process.stdout.write(`ok ${name}\n`);
}

check("reads valid saved filters and rejects malformed storage", () => {
  const run = runtime({
    stored: JSON.stringify({ range: "week", provider: "configured", model: "m1" }),
  });
  assert.equal(
    JSON.stringify(run.filters.readStoredAnalyticsFilters()),
    JSON.stringify({ range: "week", provider: "configured", model: "m1" }),
  );
  run.sessionStorage.value = "{";
  assert.equal(JSON.stringify(run.filters.readStoredAnalyticsFilters()), "{}");
  run.sessionStorage.value = "[]";
  assert.equal(JSON.stringify(run.filters.readStoredAnalyticsFilters()), "{}");
  run.sessionStorage.throwGet = true;
  assert.equal(JSON.stringify(run.filters.readStoredAnalyticsFilters()), "{}");
});

check("writes every active filter and tolerates blocked storage", () => {
  const run = runtime({ range: "week", provider: "configured", model: "m1" });
  run.elements["#analytics-model"].options.push({ value: "m1" });
  run.filters.writeAnalyticsFilters();
  assert.deepEqual(parsedStorage(run), {
    range: "week",
    provider: "configured",
    model: "m1",
  });
  run.sessionStorage.throwSet = true;
  assert.doesNotThrow(() => run.filters.writeAnalyticsFilters());
});

check("restores a valid provider before its model and persists the result", () => {
  const run = runtime();
  run.setFillHook(() => {
    const provider = run.elements["#analytics-provider"].value;
    run.elements["#analytics-model"].options = provider === "configured"
      ? [{ value: "" }, { value: "model-a" }]
      : [{ value: "" }];
  });
  run.filters.setPending({
    range: "week",
    provider: "configured",
    model: "model-a",
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-range"].value, "week");
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    range: "week",
    provider: "configured",
    model: "model-a",
  });
});

check("initializes restoration from stored session values", () => {
  const run = runtime({
    stored: JSON.stringify({
      range: "week",
      provider: "configured",
      model: "model-a",
    }),
  });
  run.setFillHook(() => {
    run.elements["#analytics-model"].options =
      run.elements["#analytics-provider"].value === "configured"
        ? [{ value: "" }, { value: "model-a" }]
        : [{ value: "" }];
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-range"].value, "week");
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.equal(run.filters.getPending(), null);
});

check("defers historical ids until provider and model inventories arrive", () => {
  const run = runtime();
  let modelAvailable = false;
  run.elements["#analytics-provider"].options = [{ value: "" }];
  run.setFillHook(() => {
    run.elements["#analytics-model"].options = modelAvailable
      ? [{ value: "" }, { value: "historical-model" }]
      : [{ value: "" }];
  });
  run.filters.setPending({
    range: "24h",
    provider: "historical-provider",
    model: "historical-model",
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), false);
  assert.notEqual(run.filters.getPending(), null);

  run.elements["#analytics-provider"].options.push({ value: "historical-provider" });
  assert.equal(
    run.filters.restoreAnalyticsFilters({
      providerInventoryComplete: true,
      modelInventoryComplete: true,
    }),
    true,
  );
  assert.equal(run.elements["#analytics-provider"].value, "historical-provider");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.notEqual(run.filters.getPending(), null);

  modelAvailable = true;
  assert.equal(
    run.filters.restoreAnalyticsFilters({ modelInventoryComplete: true }),
    true,
  );
  assert.equal(run.elements["#analytics-model"].value, "historical-model");
  assert.equal(run.filters.getPending(), null);
});

check("keeps a pending discovered model until provider discovery can add it", () => {
  const run = runtime({ provider: "configured" });
  let discovered = false;
  run.setFillHook(() => {
    run.elements["#analytics-model"].options = discovered
      ? [{ value: "" }, { value: "dynamic-model" }]
      : [{ value: "" }];
  });
  run.filters.setPending({
    range: "24h",
    provider: "configured",
    model: "dynamic-model",
  });
  assert.equal(
    run.filters.restoreAnalyticsFilters({ modelInventoryComplete: false }),
    false,
  );
  assert.notEqual(run.filters.getPending(), null);
  discovered = true;
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-model"].value, "dynamic-model");
});

check("keeps a saved provider pending when its inventory failed", () => {
  const saved = {
    range: "24h",
    provider: "configured",
    model: "model-a",
  };
  const run = runtime({ stored: JSON.stringify(saved) });
  run.elements["#analytics-provider"].options = [{ value: "" }];
  assert.equal(
    run.filters.restoreAnalyticsFilters({ providerInventoryComplete: false }),
    false,
  );
  assert.notEqual(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), saved);
  assert.equal(run.filters.settleAnalyticsInventoryChange(true), false);
  assert.deepEqual(parsedStorage(run), saved);
});

check("persists fallback when provider discovery removes the active model", () => {
  const run = runtime({
    stored: JSON.stringify({
      range: "24h",
      provider: "configured",
      model: "dynamic-model",
    }),
  });
  run.setFillHook(() => {
    run.elements["#analytics-model"].options = [
      { value: "" },
      { value: "dynamic-model" },
    ];
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  run.elements["#analytics-model"].value = "";
  assert.equal(run.filters.settleAnalyticsInventoryChange(true), true);
  assert.deepEqual(parsedStorage(run), {
    range: "24h",
    provider: "configured",
    model: "",
  });
  assert.equal(run.filters.settleAnalyticsInventoryChange(false), false);
});

check("does not restore a shared model for an invalid provider", () => {
  const run = runtime({
    stored: JSON.stringify({
      range: "24h",
      provider: "removed-provider",
      model: "shared-model",
    }),
  });
  run.setFillHook(() => {
    run.elements["#analytics-model"].options = [
      { value: "" },
      { value: "shared-model" },
    ];
  });
  assert.equal(
    run.filters.restoreAnalyticsFilters({
      providerInventoryComplete: true,
      modelInventoryComplete: true,
    }),
    false,
  );
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), { range: "24h", provider: "", model: "" });
});

check("invalid saved ids fall back and overwrite stale storage", () => {
  const run = runtime();
  run.filters.setPending({ range: "invalid", provider: "gone", model: "gone" });
  assert.equal(
    run.filters.restoreAnalyticsFilters({
      providerInventoryComplete: true,
      modelInventoryComplete: true,
    }),
    false,
  );
  assert.equal(run.elements["#analytics-range"].value, "24h");
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), { range: "24h", provider: "", model: "" });
});

check("a user save cancels pending boot restoration", () => {
  const run = runtime({ range: "1h" });
  run.filters.setPending({ range: "week", provider: "configured", model: "model-a" });
  run.filters.storeAnalyticsFilters();
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), { range: "1h", provider: "", model: "" });
});

process.stdout.write("webui analytics filters harness: all checks passed\n");
