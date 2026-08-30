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
const fillHelperSource = sourceBetween(
  "function fillAnalyticsFilters()",
  "function analyticsOptionValue(select, saved)",
);
const filterHelperSource = sourceBetween(
  "function analyticsOptionValue(select, saved)",
  "let analyticsPending =",
);

function select(value, values) {
  let current = "";
  const element = {
    options: values.map((optionValue) => ({ value: optionValue })),
    append(option) {
      this.options.push(option);
    },
    get innerHTML() {
      return "";
    },
    set innerHTML(_markup) {
      this.options = [{ value: "" }];
      current = "";
    },
    get value() {
      return current;
    },
    set value(next) {
      current = this.options.some((option) => option.value === next) ? next : "";
    },
  };
  element.value = value;
  return element;
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

function runtime({
  stored = null,
  range = "24h",
  provider = "",
  model = "",
  providers = [],
} = {}) {
  const sessionStorage = storage(stored);
  const elements = {
    "#analytics-range": select(range, ["1h", "24h", "week"]),
    "#analytics-provider": select(provider, ["", ...(provider ? [provider] : [])]),
    "#analytics-model": select(model, ["", ...(model ? [model] : [])]),
  };
  const context = {
    document: {
      createElement(tag) {
        assert.equal(tag, "option");
        return { value: "", textContent: "" };
      },
    },
    elements,
    initialProviders: providers,
    sessionStorage,
    visibilityCalls: [],
  };
  vm.runInNewContext(
    `
      "use strict";
      const ANALYTICS_FILTERS_KEY = "codex-warp-webui-analytics-filters";
      const ANALYTICS_FILTERS_VERSION = 1;
      let providers = globalThis.initialProviders;
      let analyticsProviderIds = [];
      let analyticsModelIds = [];
      let analyticsModelProvider = null;
      const $ = (selector) => globalThis.elements[selector];
      function applyAnalyticsChartVisibility(provider, model) {
        globalThis.visibilityCalls.push({ provider, model });
      }
      ${fillHelperSource}
      ${readHelperSource}
      ${restoreInitializerSource}
      ${filterHelperSource}
      globalThis.filters = {
        readStoredAnalyticsFilters,
        analyticsFiltersSnapshot,
        writeAnalyticsFilters,
        storeAnalyticsFilters,
        restoreAnalyticsFilters,
        reconcileAnalyticsFiltersAfterInventory,
        setProviders(value) { providers = value; },
        setInventory({ providerIds = [], modelIds = [], modelProvider = null }) {
          analyticsProviderIds = providerIds;
          analyticsModelIds = modelIds;
          analyticsModelProvider = modelProvider;
        },
        setPending(value) { analyticsFiltersToRestore = value; },
        getPending() { return analyticsFiltersToRestore; },
      };
    `,
    context,
  );
  return { elements, filters: context.filters, sessionStorage, visibilityCalls: context.visibilityCalls };
}

function parsedStorage(run) {
  return JSON.parse(run.sessionStorage.value);
}

function check(name, fn) {
  fn();
  process.stdout.write(`ok ${name}\n`);
}

check("accepts only complete versioned session snapshots", () => {
  const valid = {
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  };
  const run = runtime({ stored: JSON.stringify(valid) });
  assert.deepEqual(
    JSON.parse(JSON.stringify(run.filters.readStoredAnalyticsFilters())),
    valid,
  );
  for (const value of [
    null,
    "{",
    "[]",
    JSON.stringify({ ...valid, version: 2 }),
    JSON.stringify({ ...valid, model: null }),
  ]) {
    run.sessionStorage.value = value;
    assert.equal(run.filters.readStoredAnalyticsFilters(), null);
  }
  run.sessionStorage.throwGet = true;
  assert.equal(run.filters.readStoredAnalyticsFilters(), null);
});

check("stores all active filters and tolerates blocked storage", () => {
  const run = runtime({ range: "week", provider: "configured", model: "model-a" });
  run.filters.storeAnalyticsFilters("model");
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  });
  run.sessionStorage.throwSet = true;
  assert.doesNotThrow(() => run.filters.storeAnalyticsFilters("model"));
});

check("restores provider before its dependent model", () => {
  const saved = {
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  };
  const run = runtime({ stored: JSON.stringify(saved) });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-range"].value, "week");
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.notEqual(run.filters.getPending(), null);

  run.filters.setProviders([{
    id: "configured",
    display_name: "Configured",
    models: [{ id: "model-a", display_name: "Model A" }],
  }]);
  assert.equal(run.filters.reconcileAnalyticsFiltersAfterInventory(), true);
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), saved);
  const visibility = run.visibilityCalls[run.visibilityCalls.length - 1];
  assert.equal(visibility.provider, "configured");
  assert.equal(visibility.model, "model-a");
});

check("retries a saved usage option as normal analytics inventories arrive", () => {
  const saved = {
    version: 1,
    range: "24h",
    provider: "historical-provider",
    model: "historical-model",
  };
  const run = runtime({ stored: JSON.stringify(saved) });
  run.filters.setInventory({ providerIds: ["historical-provider"], modelProvider: "" });
  assert.equal(run.filters.reconcileAnalyticsFiltersAfterInventory(), true);
  assert.equal(run.elements["#analytics-provider"].value, "historical-provider");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.notEqual(run.filters.getPending(), null);

  run.filters.setInventory({
    providerIds: ["historical-provider"],
    modelIds: ["historical-model"],
    modelProvider: "historical-provider",
  });
  assert.equal(run.filters.reconcileAnalyticsFiltersAfterInventory(), true);
  assert.equal(run.elements["#analytics-model"].value, "historical-model");
  assert.equal(run.filters.getPending(), null);
});

check("unavailable saved options leave effective defaults without breaking loading", () => {
  const saved = {
    version: 1,
    range: "week",
    provider: "missing-provider",
    model: "missing-model",
  };
  const run = runtime({ stored: JSON.stringify(saved) });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-range"].value, "week");
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.notEqual(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), saved);
});

check("a range edit preserves unresolved provider and model fields", () => {
  const run = runtime({ range: "1h" });
  run.filters.setPending({
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  });
  run.filters.storeAnalyticsFilters("range");
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getPending())), {
    version: 1,
    range: "1h",
    provider: "configured",
    model: "model-a",
  });
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "1h",
    provider: "configured",
    model: "model-a",
  });
});

check("an explicit provider choice cancels pending restoration", () => {
  const run = runtime({ provider: "configured" });
  run.filters.setPending({
    version: 1,
    range: "week",
    provider: "old-provider",
    model: "old-model",
  });
  run.filters.storeAnalyticsFilters("provider");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "24h",
    provider: "configured",
    model: "",
  });
});

check("an explicit model choice cancels pending restoration", () => {
  const run = runtime({ provider: "configured", model: "model-a" });
  run.filters.setPending({
    version: 1,
    range: "week",
    provider: "configured",
    model: "old-model",
  });
  run.filters.storeAnalyticsFilters("model");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "24h",
    provider: "configured",
    model: "model-a",
  });
});

process.stdout.write("webui analytics filters harness: all checks passed\n");
