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
const discoveryHelperSource = sourceBetween(
  "async function refreshModelRoutes()",
  "async function loadProviders(",
);
const productionFillHelperSource = sourceBetween(
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
  productionFill = false,
} = {}) {
  const sessionStorage = storage(stored);
  const elements = {
    "#analytics-range": select(range, ["1h", "24h", "week"]),
    "#analytics-provider": select(
      provider,
      ["", "configured", ...(provider && provider !== "configured" ? [provider] : [])],
    ),
    "#analytics-model": select(model, ["", ...(model ? [model] : [])]),
  };
  const fillSource = productionFill
    ? productionFillHelperSource
    : "function fillAnalyticsFilters() { return globalThis.fillHook(); }";
  const context = {
    document: {
      createElement(tag) {
        assert.equal(tag, "option");
        return { value: "", textContent: "" };
      },
    },
    elements,
    fillHook: () => {},
    initialProviders: providers,
    sessionStorage,
  };
  vm.runInNewContext(
    `
      "use strict";
      const ANALYTICS_FILTERS_KEY = "codex-warp-webui-analytics-filters";
      const ANALYTICS_FILTERS_VERSION = 1;
      let analyticsProviderIds = [];
      let analyticsModelIds = [];
      let analyticsModelProvider = null;
      let analyticsProviderInventoryLoaded = false;
      let analyticsModelInventoryLoaded = false;
      let providerInventoryLoaded = false;
      let providerModelInventoryLoaded = false;
      let providerDiscoveryInFlight = false;
      let providers = globalThis.initialProviders;
      const $ = (selector) => globalThis.elements[selector];
      ${fillSource}
      ${readHelperSource}
      ${restoreInitializerSource}
      ${filterHelperSource}
      globalThis.filters = {
        readStoredAnalyticsFilters,
        analyticsFiltersSnapshot,
        writeAnalyticsFilters,
        storeAnalyticsFilters,
        restoreAnalyticsFilters,
        settleAnalyticsInventoryChange,
        updateAnalyticsIdentityInventories,
        analyticsNeedsIdentityInventories,
        analyticsModelInventoryIsComplete,
        rebuildInventory() { return fillAnalyticsFilters(); },
        setInventory({
          providerIds = [],
          modelIds = [],
          modelProvider = null,
          providerLoaded = true,
          modelLoaded = true,
        }) {
          analyticsProviderIds = providerIds;
          analyticsModelIds = modelIds;
          analyticsModelProvider = modelProvider;
          analyticsProviderInventoryLoaded = providerLoaded;
          analyticsModelInventoryLoaded = modelLoaded;
        },
        setProviders(value) { providers = value; },
        setProviderInventoryState({
          providerLoaded = false,
          modelLoaded = false,
          discoveryInFlight = false,
        }) {
          providerInventoryLoaded = providerLoaded;
          providerModelInventoryLoaded = modelLoaded;
          providerDiscoveryInFlight = discoveryInFlight;
        },
        setPending(value) { analyticsFiltersToRestore = value; },
        getPending() { return analyticsFiltersToRestore; },
        getInventory() {
          return JSON.parse(JSON.stringify({
            providerIds: analyticsProviderIds,
            modelIds: analyticsModelIds,
            modelProvider: analyticsModelProvider,
          }));
        },
        getInventoryLoaded() {
          return {
            provider: analyticsProviderInventoryLoaded,
            model: analyticsModelInventoryLoaded,
          };
        },
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
    version: 1,
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
    version: 1,
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
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  });
});

check("initializes restoration from stored session values", () => {
  const run = runtime({
    stored: JSON.stringify({
      version: 1,
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

check("restores a historical choice from actual analytics inventories", () => {
  const run = runtime({
    stored: JSON.stringify({
      version: 1,
      range: "1h",
      provider: "historical-provider",
      model: "historical-model",
    }),
  });
  run.elements["#analytics-provider"].options = [{ value: "" }];
  run.filters.setInventory({
    providerIds: ["historical-provider"],
    modelIds: ["historical-model"],
    modelProvider: "historical-provider",
  });
  run.setFillHook(() => {
    const inventory = run.filters.getInventory();
    const provider = run.elements["#analytics-provider"];
    const model = run.elements["#analytics-model"];
    provider.options = ["", ...inventory.providerIds].map((value) => ({ value }));
    model.options = inventory.modelProvider === provider.value
      ? ["", ...inventory.modelIds].map((value) => ({ value }))
      : [{ value: "" }];
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-range"].value, "1h");
  assert.equal(run.elements["#analytics-provider"].value, "historical-provider");
  assert.equal(run.elements["#analytics-model"].value, "historical-model");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "1h",
    provider: "historical-provider",
    model: "historical-model",
  });
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
    version: 1,
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
    version: 1,
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
    version: 1,
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
      version: 1,
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
    version: 1,
    range: "24h",
    provider: "configured",
    model: "",
  });
  assert.equal(run.filters.settleAnalyticsInventoryChange(false), false);
});

check("does not restore a shared model for an invalid provider", () => {
  const run = runtime({
    stored: JSON.stringify({
      version: 1,
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
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "24h",
    provider: "",
    model: "",
  });
});

check("versioned stale ids fall back and overwrite storage", () => {
  const run = runtime({ productionFill: true });
  run.filters.setPending({
    version: 1,
    range: "invalid",
    provider: "gone",
    model: "gone",
  });
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
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "24h",
    provider: "",
    model: "",
  });
});

check("unsupported storage versions fall back without restoring values", () => {
  const run = runtime({
    stored: JSON.stringify({
      version: 999,
      range: "week",
      provider: "configured",
      model: "model-a",
    }),
    providers: [{
      id: "configured",
      display_name: "Configured",
      models: [{ id: "model-a", display_name: "Model A" }],
    }],
    productionFill: true,
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), false);
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "24h",
    provider: "",
    model: "",
  });
});

check("a range change preserves untouched pending provider and model", () => {
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
  run.setFillHook(() => {
    run.elements["#analytics-model"].options =
      run.elements["#analytics-provider"].value === "configured"
        ? [{ value: "" }, { value: "model-a" }]
        : [{ value: "" }];
  });
  assert.equal(run.filters.restoreAnalyticsFilters(), true);
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.equal(run.filters.getPending(), null);
});

check("an explicit provider change cancels pending restoration", () => {
  const run = runtime({ range: "1h" });
  run.filters.setPending({
    version: 1,
    range: "week",
    provider: "historical-provider",
    model: "historical-model",
  });
  run.elements["#analytics-provider"].value = "configured";
  run.filters.storeAnalyticsFilters("provider");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "1h",
    provider: "configured",
    model: "",
  });
});

check("production fill preserves a saved selection across inventory rebuilds", () => {
  const run = runtime({
    range: "week",
    provider: "configured",
    providers: [{
      id: "configured",
      display_name: "Configured",
      models: [{ id: "model-a", display_name: "Model A" }],
    }],
    productionFill: true,
  });
  run.elements["#analytics-model"].options.push({ value: "model-a" });
  run.elements["#analytics-model"].value = "model-a";
  run.filters.setInventory({
    providerIds: ["configured"],
    modelIds: ["model-a"],
    modelProvider: "configured",
  });
  run.filters.storeAnalyticsFilters();
  run.filters.setProviders([{
    id: "configured",
    display_name: "Configured",
    models: [],
  }]);
  const inventoryChanged = run.filters.rebuildInventory();
  assert.equal(inventoryChanged, false);
  assert.equal(run.filters.settleAnalyticsInventoryChange(inventoryChanged), false);
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "week",
    provider: "configured",
    model: "model-a",
  });
});

check("production fill reports removed options and persists the fallback", () => {
  const run = runtime({
    range: "week",
    provider: "configured",
    providers: [{
      id: "configured",
      display_name: "Configured",
      models: [{ id: "model-a", display_name: "Model A" }],
    }],
    productionFill: true,
  });
  run.elements["#analytics-model"].options.push({ value: "model-a" });
  run.elements["#analytics-model"].value = "model-a";
  run.filters.storeAnalyticsFilters();
  const previousFilters = JSON.parse(JSON.stringify(run.filters.analyticsFiltersSnapshot()));
  run.filters.setProviders([]);
  run.filters.setInventory({ modelProvider: "" });
  const inventoryChanged = run.filters.rebuildInventory();
  assert.equal(inventoryChanged, true);
  assert.equal(
    run.filters.settleAnalyticsInventoryChange(inventoryChanged, previousFilters),
    true,
  );
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.equal(run.elements["#analytics-model"].value, "");
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "week",
    provider: "",
    model: "",
  });
});

check("inventory fallback waits for all-history identities", () => {
  const run = runtime({
    range: "1h",
    provider: "configured",
    providers: [{
      id: "configured",
      display_name: "Configured",
      models: [{ id: "model-a", display_name: "Model A" }],
    }],
    productionFill: true,
  });
  run.elements["#analytics-model"].options.push({ value: "model-a" });
  run.elements["#analytics-model"].value = "model-a";
  run.filters.storeAnalyticsFilters();
  const previousFilters = JSON.parse(JSON.stringify(run.filters.analyticsFiltersSnapshot()));
  run.filters.setProviders([]);
  const inventoryChanged = run.filters.rebuildInventory();
  assert.equal(inventoryChanged, true);
  assert.equal(
    run.filters.settleAnalyticsInventoryChange(inventoryChanged, previousFilters),
    true,
  );
  assert.deepEqual(parsedStorage(run), previousFilters);
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getPending())), previousFilters);

  run.filters.setInventory({
    providerIds: ["configured"],
    modelProvider: "",
  });
  assert.equal(
    run.filters.restoreAnalyticsFilters({ providerInventoryComplete: true }),
    true,
  );
  assert.equal(run.elements["#analytics-provider"].value, "configured");
  assert.notEqual(run.filters.getPending(), null);

  run.filters.setInventory({
    providerIds: ["configured"],
    modelIds: ["model-a"],
    modelProvider: "configured",
  });
  assert.equal(
    run.filters.restoreAnalyticsFilters({
      providerInventoryComplete: true,
      modelInventoryComplete: true,
    }),
    true,
  );
  assert.equal(run.elements["#analytics-model"].value, "model-a");
  assert.equal(run.filters.getPending(), null);
  assert.deepEqual(parsedStorage(run), previousFilters);
});

check("ordinary analytics breakdowns populate current-range filter options", () => {
  const run = runtime({ productionFill: true });
  run.filters.setPending(null);
  run.filters.updateAnalyticsIdentityInventories({
    by_provider: [{ key: "historical-provider" }],
    by_model: [{ key: "historical-model" }],
  }, "", "");
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventory())), {
    providerIds: ["historical-provider"],
    modelIds: ["historical-model"],
    modelProvider: "",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventoryLoaded())), {
    provider: false,
    model: false,
  });
  run.filters.rebuildInventory();
  assert.equal(
    run.elements["#analytics-provider"].options.some(
      (option) => option.value === "historical-provider",
    ),
    true,
  );
  assert.equal(
    run.elements["#analytics-model"].options.some(
      (option) => option.value === "historical-model",
    ),
    true,
  );
});

check("authoritative identities survive later ordinary responses", () => {
  const run = runtime();
  run.filters.updateAnalyticsIdentityInventories({
    provider_ids: ["historical-provider"],
    model_ids: ["historical-model"],
  }, "", "");
  run.filters.updateAnalyticsIdentityInventories({
    by_provider: [],
    by_model: [],
  }, "", "");
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventory())), {
    providerIds: ["historical-provider"],
    modelIds: ["historical-model"],
    modelProvider: "",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventoryLoaded())), {
    provider: true,
    model: true,
  });
});

check("identity requests stop after the relevant scopes load", () => {
  const run = runtime();
  run.filters.setPending({
    version: 1,
    range: "24h",
    provider: "historical-provider",
    model: "historical-model",
  });
  assert.equal(run.filters.analyticsNeedsIdentityInventories(""), true);
  run.filters.updateAnalyticsIdentityInventories({
    provider_ids: ["historical-provider"],
    model_ids: ["historical-model"],
  }, "", "");
  assert.equal(run.filters.analyticsNeedsIdentityInventories(""), false);
  assert.equal(
    run.filters.analyticsNeedsIdentityInventories("historical-provider"),
    true,
  );
  run.filters.updateAnalyticsIdentityInventories({
    provider_ids: ["historical-provider"],
    model_ids: ["historical-model"],
  }, "historical-provider", "");
  assert.equal(
    run.filters.analyticsNeedsIdentityInventories("historical-provider"),
    false,
  );

  const configured = runtime({ provider: "configured" });
  configured.filters.setPending({
    version: 1,
    range: "24h",
    provider: "configured",
    model: "missing-model",
  });
  assert.equal(
    configured.filters.analyticsNeedsIdentityInventories("configured"),
    true,
  );
  configured.filters.updateAnalyticsIdentityInventories({
    provider_ids: ["configured"],
    model_ids: ["other-model"],
  }, "configured", "");
  assert.equal(
    configured.filters.analyticsNeedsIdentityInventories("configured"),
    false,
  );
  assert.equal(configured.filters.getInventoryLoaded().provider, false);
});

check("deleted providers do not wait on failed live model discovery", () => {
  const run = runtime({ provider: "historical-provider" });
  run.filters.setProviderInventoryState({ providerLoaded: true });
  run.filters.setInventory({
    providerIds: ["historical-provider"],
    modelIds: ["historical-model"],
    modelProvider: "historical-provider",
  });
  assert.equal(
    run.filters.analyticsModelInventoryIsComplete("historical-provider", ""),
    true,
  );
  assert.equal(run.filters.analyticsModelInventoryIsComplete("", ""), false);
  run.filters.setProviders([{ id: "historical-provider", models: [] }]);
  assert.equal(
    run.filters.analyticsModelInventoryIsComplete("historical-provider", ""),
    false,
  );
});

check("user saves do not manufacture analytics inventory", () => {
  const run = runtime({
    range: "week",
    provider: "provider-a",
    model: "model-a",
  });
  run.elements["#analytics-provider"].options.push(
    { value: "provider-a" },
    { value: "provider-b" },
  );
  run.elements["#analytics-model"].options.push({ value: "model-a" });
  run.filters.storeAnalyticsFilters();
  run.elements["#analytics-provider"].value = "provider-b";
  run.elements["#analytics-model"].options = [
    { value: "" },
    { value: "model-b" },
  ];
  run.elements["#analytics-model"].value = "model-b";
  run.filters.storeAnalyticsFilters();
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventory())), {
    providerIds: [],
    modelIds: [],
    modelProvider: null,
  });
});

check("a global model save survives a same-page inventory rebuild", () => {
  const run = runtime({
    range: "week",
    provider: "",
    model: "global-model",
    productionFill: true,
  });
  run.elements["#analytics-model"].options.push({ value: "global-model" });
  run.filters.setInventory({ modelIds: ["global-model"], modelProvider: "" });
  run.filters.storeAnalyticsFilters();
  assert.deepEqual(JSON.parse(JSON.stringify(run.filters.getInventory())), {
    providerIds: [],
    modelIds: ["global-model"],
    modelProvider: "",
  });
  const inventoryChanged = run.filters.rebuildInventory();
  assert.equal(inventoryChanged, false);
  assert.equal(run.filters.settleAnalyticsInventoryChange(inventoryChanged), false);
  assert.equal(run.elements["#analytics-provider"].value, "");
  assert.equal(run.elements["#analytics-model"].value, "global-model");
  assert.deepEqual(parsedStorage(run), {
    version: 1,
    range: "week",
    provider: "",
    model: "global-model",
  });
});

async function discoveryResult(fetch) {
  const context = {
    AbortController: class {
      constructor() { this.signal = {}; }
      abort() {}
    },
    clearTimeout() {},
    fetch,
    setTimeout() { return 1; },
  };
  vm.runInNewContext(
    `${discoveryHelperSource}\n` +
      "globalThis.refreshModelRoutes = refreshModelRoutes;",
    context,
  );
  return context.refreshModelRoutes();
}

Promise.resolve()
  .then(async () => {
    assert.equal(await discoveryResult(async () => ({ ok: true })), true);
    assert.equal(await discoveryResult(async () => ({ ok: false })), false);
    assert.equal(
      await discoveryResult(async () => { throw new Error("discovery unavailable"); }),
      false,
    );
    process.stdout.write("ok accepts only successful model discovery responses\n");
    process.stdout.write("webui analytics filters harness: all checks passed\n");
  })
  .catch((error) => {
    process.exitCode = 1;
    console.error(error);
  });
