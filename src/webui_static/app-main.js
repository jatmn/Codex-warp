(() => {
  "use strict";

  // Fragment: /ui/app.js prefixes footer-status.js in the same response so the
  // overlay cannot 404 independently. This file is not a complete entry.

  const API = "/api";
  const TOKEN_KEY = "codex-warp-webui-token";
  const ANALYTICS_FILTERS_KEY = "codex-warp-webui-analytics-filters";
  const ANALYTICS_FILTERS_VERSION = 1;
  function readStoredToken() {
    try { return sessionStorage.getItem(TOKEN_KEY) || ""; } catch { return ""; }
  }
  function storeToken(token) {
    try { sessionStorage.setItem(TOKEN_KEY, token); } catch { /* optional persistence */ }
  }
  function clearStoredToken() {
    try { sessionStorage.removeItem(TOKEN_KEY); } catch { /* optional persistence */ }
  }
  function readStoredAnalyticsFilters() {
    try {
      const raw = sessionStorage.getItem(ANALYTICS_FILTERS_KEY);
      if (!raw) return null;
      const parsed = JSON.parse(raw);
      return parsed?.version === ANALYTICS_FILTERS_VERSION &&
        typeof parsed.range === "string" &&
        typeof parsed.provider === "string" &&
        typeof parsed.model === "string"
        ? parsed
        : null;
    } catch {
      return null;
    }
  }
  let managementToken = readStoredToken();
  let analyticsFiltersToRestore = readStoredAnalyticsFilters();
  let managementTokenPrompt = null;
  let providers = [];
  let providerTemplates = [];
  let selectedTemplateCatalog = [];
  let analyticsProviderIds = [];
  let analyticsModelIds = [];
  let analyticsModelProvider = null;
  let analyticsTimer = null;
  let analyticsInFlight = false;
  let analyticsSnapshot = null;
  let logsTimer = null;
  let logsInFlight = false;
  let logsPending = false;
  let logsExpanded = new Set();
  let loggingSettingsHydrated = false;
  let loggingFormDirty = false;
  let loggingHydrating = false;
  let bootComplete = false;
  let bootFooterHold = false;
  let tabEpoch = 0;
  const expandedProviderIds = new Set();
  const refreshingProviderIds = new Set();
  const VALID_TABS = new Set(["analytics", "providers", "logs"]);
  function tabFromLocation() {
    const hash = location.hash.replace(/^#/, "");
    return VALID_TABS.has(hash) ? hash : "analytics";
  }
  // Hash is the source of truth before boot paints panels. Defaulting to
  // analytics here made chart-math failure write the footer on #providers.
  let activeTab = tabFromLocation();

  const $ = (sel) => document.querySelector(sel);
  const Footer = globalThis.CodexWarpFooter;
  if (!Footer || typeof Footer.analyticsDisplayStatus !== "function") {
    throw new Error("codex-warp footer status failed to load");
  }
  function writeStatus(msg) {
    $("#status-line").textContent = msg;
  }
  function footerText(msg, opts) {
    const isError = !!(opts && opts.isError);
    const remap = !opts || opts.remap !== false;
    const math = globalThis.CodexWarpCharts;
    return Footer.analyticsDisplayStatus(
      !!math,
      activeTab,
      msg,
      isError,
      Footer.chartsFailedStatus,
      remap,
    );
  }
  function commitStatus(msg, opts) {
    writeStatus(footerText(msg, opts));
  }
  // One footer slot. Background polls must not replace a failed boot.
  // User-initiated actions (save, filter change, provider edit) own the
  // footer and clear that hold. Chart-math failure is not a boot hold: it
  // remaps analytics-tab copy through footerText and leaves other tabs alone.
  const status = (msg, opts) => {
    bootFooterHold = false;
    commitStatus(msg, opts);
  };
  function pollStatus(msg, opts) {
    if (bootFooterHold) return;
    commitStatus(msg, opts);
  }

  function formatErrorMessage(err) {
    return err instanceof Error ? err.message : String(err);
  }

  // Empty → JSON null (clear to defaults). Invalid input must not become
  // `Number(...)` NaN: JSON.stringify(NaN) is `null`, which the API treats as
  // Clear and silently resets rotation limits.
  function optionalPositiveInt(raw, label) {
    const text = String(raw ?? "").trim();
    if (!text) return null;
    if (!/^[0-9]+$/.test(text)) {
      throw new Error(`${label} must be a positive integer`);
    }
    const value = Number(text);
    if (!Number.isSafeInteger(value) || value < 1) {
      throw new Error(`${label} must be a positive integer`);
    }
    return value;
  }

  const themeApi = window.installCodexWarpTheme(window);
  if (!themeApi) {
    throw new Error("codex-warp theme bootstrap failed to load");
  }

  function applyTheme(theme, { persist = false } = {}) {
    themeApi.apply(theme, { persist });
  }

  $("#theme-toggle")?.addEventListener("click", () => {
    const next = themeApi.getApplied() === "dark" ? "light" : "dark";
    applyTheme(next, { persist: true });
  });

  function svgIcon(paths) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    for (const d of paths) {
      const pathEl = document.createElementNS("http://www.w3.org/2000/svg", "path");
      pathEl.setAttribute("d", d);
      svg.append(pathEl);
    }
    return svg;
  }
  const ICONS = {
    chevron: ["M6 9l6 6 6-6"],
    trash: ["M3 6h18", "M8 6V4h8v2", "M19 6l-1 14H6L5 6", "M10 11v6M14 11v6"],
  };

  function promptForManagementToken() {
    if (!managementTokenPrompt) {
      // Publish the promise before prompting so every concurrent 401 joins the
      // same authentication challenge instead of opening its own dialog.
      managementTokenPrompt = Promise.resolve()
        .then(() => window.prompt("This Codex Warp server requires a Web UI token."))
        .then((token) => {
          if (token) {
            managementToken = token;
            storeToken(token);
          }
          return token;
        })
        .finally(() => { managementTokenPrompt = null; });
    }
    return managementTokenPrompt;
  }

  async function api(path, opts = {}, allowAuthRetry = true) {
    const headers = { "Content-Type": "application/json", ...(opts.headers || {}) };
    const tokenUsed = managementToken;
    if (tokenUsed) headers.Authorization = `Bearer ${tokenUsed}`;
    const res = await fetch(API + path, {
      ...opts,
      headers,
    });
    if (res.status === 401 && allowAuthRetry) {
      // Another request may already have completed the shared challenge while
      // this tokenless request was in flight.
      if (managementToken && managementToken !== tokenUsed) {
        return api(path, opts, false);
      }
      const token = await promptForManagementToken();
      if (token) {
        return api(path, opts, false);
      }
    }
    const text = await res.text();
    let data = null;
    try { data = text ? JSON.parse(text) : null; } catch { data = { error: text }; }
    if (!res.ok) {
      // Do not let a stale in-flight request erase a newer valid credential.
      if (res.status === 401 && managementToken === tokenUsed) {
        managementToken = "";
        clearStoredToken();
      }
      throw new Error(data?.error || res.statusText);
    }
    return data;
  }

  function tabHash(name) {
    return name === "analytics" ? "" : `#${name}`;
  }

  function syncTabHash(name) {
    const hash = tabHash(name);
    const url = `${location.pathname}${location.search}${hash}`;
    if (location.hash !== hash) {
      history.replaceState(null, "", url);
    }
  }

  function showTabPanel(name) {
    if (!VALID_TABS.has(name)) {
      return;
    }
    const previous = activeTab;
    activeTab = name;
    document.querySelectorAll(".tab").forEach((b) => {
      const on = b.dataset.tab === name;
      b.classList.toggle("active", on);
      b.setAttribute("aria-selected", on ? "true" : "false");
    });
    document.querySelectorAll(".panel").forEach((p) => {
      const on = p.id === `panel-${name}`;
      p.classList.toggle("active", on);
      p.hidden = !on;
    });
    syncTabHash(name);
    if (previous === "analytics" && name !== "analytics") {
      deactivateCharts();
    }
    if (name === "analytics") {
      renderAnalyticsPresentation();
      scheduleChartResize();
    }
  }

  async function activateTabPolls(name) {
    const epoch = ++tabEpoch;
    stopAnalyticsPoll();
    stopLogsPoll();
    if (name === "analytics") {
      await startAnalyticsPoll(epoch);
      return;
    }
    if (name === "logs") {
      await startLogsPoll({ epoch });
      return;
    }
    if (epoch === tabEpoch) pollStatus("Ready");
  }

  function switchTab(name) {
    if (!VALID_TABS.has(name)) {
      return;
    }
    showTabPanel(name);
    if (bootComplete) {
      void activateTabPolls(name);
    }
  }

  document.querySelectorAll(".tab").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.dataset.tab));
  });

  window.addEventListener("hashchange", () => {
    const name = tabFromLocation();
    if (name !== activeTab) {
      switchTab(name);
    }
  });

  function toggleSwitch(onChange) {
    const wrap = document.createElement("label");
    wrap.className = "switch";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.addEventListener("change", () => onChange(input.checked));
    const span = document.createElement("span");
    wrap.append(input, span);
    return { wrap, input };
  }

  async function refreshModelRoutes() {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 10000);
    try {
      await fetch("/v1/models", { signal: controller.signal });
      return true;
    } catch {
      // Best-effort: populate server model_routes for discovered upstream models.
      return false;
    } finally {
      clearTimeout(timeout);
    }
  }

  async function loadProviders({ refreshRoutes = true, updateStatus = true } = {}) {
    if (updateStatus) status("Loading providers…");
    // Local persisted/configured providers must render before live discovery:
    // a stalled upstream must not block the controls needed to disable it.
    providers = await api("/providers");
    renderProviders();
    refreshRestoredAnalytics(reconcileAnalyticsFiltersAfterInventory());
    if (refreshRoutes) {
      // Mutations refresh routes server-side. Initial discovery is best-effort
      // background enrichment and republishes the provider view when complete.
      void refreshModelRoutes().then(async (refreshed) => {
        if (!refreshed) return;
        try {
          providers = await api("/providers");
          renderProviders();
          refreshRestoredAnalytics(reconcileAnalyticsFiltersAfterInventory());
        } catch {
          // The already-rendered local management view remains usable.
        }
      });
    }
  }

  function renderProviders() {
    const list = $("#providers-list");
    list.innerHTML = "";
    if (!providers.length) {
      list.innerHTML = "<p class='muted'>No providers configured.</p>";
      return;
    }
    for (const p of providers) {
      const card = document.createElement("article");
      card.className = "provider-card";
      const head = document.createElement("div");
      head.className = "provider-head";

      const title = document.createElement("div");
      title.className = "provider-title";
      const name = document.createElement("strong");
      name.textContent = p.display_name ?? "";
      const sub = document.createElement("span");
      sub.textContent = `${p.id ?? ""} · ${p.base_url ?? ""}`;
      title.append(name, sub);

      const sw = toggleSwitch(async (enabled) => {
        try {
          await api(`/providers/${encodeURIComponent(p.id)}/enabled`, {
            method: "POST",
            body: JSON.stringify({ enabled }),
          });
          p.enabled = enabled;
          await loadProviders({ refreshRoutes: false });
          status(`${p.id} ${enabled ? "enabled" : "disabled"}`);
        } catch (e) {
          sw.input.checked = !enabled;
          status(`Error: ${formatErrorMessage(e)}`);
        }
      });
      sw.input.checked = p.enabled;

      const isExpanded = expandedProviderIds.has(p.id);
      const models = document.createElement("div");
      models.className = isExpanded ? "models" : "models collapsed";
      if (isExpanded) {
        card.classList.add("expanded");
      }

      const expandBtn = document.createElement("button");
      expandBtn.type = "button";
      expandBtn.className = "btn icon provider-chevron";
      expandBtn.append(svgIcon(ICONS.chevron));
      expandBtn.title = isExpanded ? "Hide models" : "Show models";
      expandBtn.setAttribute("aria-label", `Toggle models for ${p.display_name || p.id}`);
      expandBtn.setAttribute("aria-expanded", isExpanded ? "true" : "false");
      expandBtn.addEventListener("click", () => {
        const collapsed = models.classList.toggle("collapsed");
        card.classList.toggle("expanded", !collapsed);
        if (collapsed) {
          expandedProviderIds.delete(p.id);
        } else {
          expandedProviderIds.add(p.id);
        }
        expandBtn.setAttribute("aria-expanded", collapsed ? "false" : "true");
        expandBtn.title = collapsed ? "Show models" : "Hide models";
      });

      const editBtn = document.createElement("button");
      editBtn.type = "button";
      editBtn.className = "btn small";
      editBtn.textContent = "Edit";
      editBtn.addEventListener("click", () => {
        void openProviderForm(p);
      });

      const delBtn = document.createElement("button");
      delBtn.type = "button";
      delBtn.className = "btn small danger";
      delBtn.textContent = "Delete";
      delBtn.addEventListener("click", async () => {
        if (!confirm(`Delete provider ${p.id}?`)) return;
        try {
          await api(`/providers/${encodeURIComponent(p.id)}`, { method: "DELETE" });
          expandedProviderIds.delete(p.id);
          await loadProviders({ refreshRoutes: false });
        } catch (e) { status(`Error: ${formatErrorMessage(e)}`); }
      });

      const addModelBtn = document.createElement("button");
      addModelBtn.type = "button";
      addModelBtn.className = "btn small";
      addModelBtn.textContent = "Add model";
      addModelBtn.addEventListener("click", () => openModelForm(p.id));

      const actions = document.createElement("div");
      actions.className = "provider-actions";
      actions.append(editBtn, addModelBtn);
      // The primary provider is process/bootstrap configuration and the API
      // intentionally does not support deleting that identity.
      if (p.id !== "default") actions.append(delBtn);

      head.append(title, sw.wrap, actions, expandBtn);
      renderModels(p, models);
      card.append(head, models);
      list.append(card);
    }
  }

  function renderModels(provider, container) {
    container.replaceChildren();
    const models = provider.models || [];
    const head = document.createElement("div");
    head.className = "models-head";
    const label = document.createElement("p");
    label.className = "models-label";
    label.textContent = "Models";
    head.append(label);
    if (!provider.model_catalog_only) {
      const refreshBtn = document.createElement("button");
      refreshBtn.type = "button";
      refreshBtn.className = "btn small";
      const isRefreshing = refreshingProviderIds.has(provider.id);
      refreshBtn.textContent = isRefreshing ? "Refreshing..." : "Refresh";
      refreshBtn.disabled = !provider.enabled || isRefreshing;
      refreshBtn.title = provider.enabled
        ? "Refresh models from the provider API"
        : "Enable the provider before refreshing models";
      refreshBtn.setAttribute(
        "aria-label",
        `Refresh models for ${provider.display_name || provider.id}`,
      );
      refreshBtn.addEventListener("click", async () => {
        if (refreshingProviderIds.has(provider.id)) {
          return;
        }
        refreshingProviderIds.add(provider.id);
        refreshBtn.disabled = true;
        refreshBtn.textContent = "Refreshing...";
        status(`Refreshing models for ${provider.id}...`);
        let apiError = null;
        let reloadError = null;
        try {
          await api(`/providers/${encodeURIComponent(provider.id)}/refresh-models`, {
            method: "POST",
            body: JSON.stringify({}),
          });
        } catch (error) {
          apiError = error;
        }
        try {
          await loadProviders({ refreshRoutes: false, updateStatus: false });
        } catch (error) {
          reloadError = error;
        } finally {
          refreshingProviderIds.delete(provider.id);
          renderProviders();
        }
        if (apiError && reloadError) {
          status(
            `Error: ${formatErrorMessage(apiError)}. Could not reload providers: ${formatErrorMessage(reloadError)}`,
          );
        } else if (apiError) {
          status(`Error: ${formatErrorMessage(apiError)}`);
        } else if (reloadError) {
          status(
            `Refreshed models for ${provider.id}, but could not reload providers: ${formatErrorMessage(reloadError)}`,
          );
        } else {
          status(`Refreshed models for ${provider.id}`);
        }
      });
      head.append(refreshBtn);
    }
    container.append(head);
    if (!models.length) {
      const empty = document.createElement("p");
      empty.className = "muted";
      empty.textContent = "No models.";
      container.append(empty);
      return;
    }
    for (const m of models) {
      const row = document.createElement("div");
      row.className = "model-row";
      const meta = document.createElement("div");
      meta.className = "model-meta";
      const name = document.createElement("strong");
      name.textContent = m.display_name || m.id || "";
      const idEl = document.createElement("small");
      idEl.textContent = m.id ?? "";
      const reasoning = document.createElement("small");
      const modes = Array.isArray(m.supported_reasoning_levels)
        ? m.supported_reasoning_levels.join(", ")
        : "none";
      reasoning.textContent = `Reasoning: ${modes} (default: ${m.default_reasoning_level || "none"})`;
      meta.append(name, idEl, reasoning);
      const sw = toggleSwitch(async (enabled) => {
        try {
          const view = await api(
            `/providers/${encodeURIComponent(provider.id)}/models/enabled/${encodeURIComponent(m.id)}`,
            { method: "POST", body: JSON.stringify({ enabled }) },
          );
          m.enabled = view.enabled;
          // Route ownership can move across providers when shared slugs toggle;
          // reload the full provider list so sibling cards stay accurate.
          // Server already rebuilt routes during the enable API call.
          await loadProviders({ refreshRoutes: false });
        } catch (e) {
          sw.input.checked = !enabled;
          status(`Error: ${formatErrorMessage(e)}`);
        }
      });
      sw.input.checked = m.enabled;
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn icon danger";
      del.append(svgIcon(ICONS.trash));
      del.title = `Remove ${m.id}`;
      del.setAttribute("aria-label", `Remove model ${m.id}`);
      del.addEventListener("click", async () => {
        if (!confirm(`Remove model ${m.id}?`)) return;
        try {
          await api(
            `/providers/${encodeURIComponent(provider.id)}/models/${encodeURIComponent(m.id)}`,
            { method: "DELETE" },
          );
          await loadProviders({ refreshRoutes: false });
        } catch (e) { status(`Error: ${formatErrorMessage(e)}`); }
      });
      const actions = [sw.wrap];
      const edit = document.createElement("button");
      edit.type = "button";
      edit.className = "btn small";
      edit.textContent = "Edit";
      edit.addEventListener("click", () => openModelForm(provider.id, m));
      actions.push(edit);
      actions.push(del);
      row.append(meta, ...actions);
      container.append(row);
    }
  }

  const providerDialog = $("#provider-dialog");
  const providerForm = $("#provider-form");
  const templateSelect = $("#provider-template");
  const templateDescription = $("#template-description");
  const templateCatalogPreview = $("#template-catalog-preview");
  const templateField = $("#template-field");
  const providerIdInput = providerForm.querySelector("[name=id]");
  const apiKeyInput = providerForm.querySelector("[name=api_key_env]");
  const providerHeadersSection = $("#provider-headers");
  const providerHeadersRows = $("#provider-headers-rows");
  const addProviderHeaderBtn = $("#provider-headers-add");

  const clearCredentialsBtn = $("#provider-clear-credentials");

  // Keep in lockstep with looks_like_env_var_name in src/webui.rs.
  function looksLikeEnvVarName(value) {
    if (!value) return false;
    if (!/^[A-Z_][A-Z0-9_]*$/.test(value)) return false;
    return value.includes("_");
  }

  // Keep in lockstep with mask_api_key in src/webui.rs.
  function maskApiKey(value) {
    const chars = Array.from(value);
    const n = chars.length;
    if (!n) return "";
    let prefix = 4;
    let suffix = 4;
    if (n <= 8) {
      prefix = 1;
      suffix = 1;
    } else if (n <= 12) {
      prefix = 2;
      suffix = 2;
    }
    if (prefix + suffix >= n) {
      return "•".repeat(n);
    }
    return chars.slice(0, prefix).join("")
      + "•".repeat(n - prefix - suffix)
      + chars.slice(n - suffix).join("");
  }

  // loadedKind is the credential class at form open, not a snapshot of the
  // current string. Inline secrets cannot be edited in place (the operator
  // never has the raw value); env names stay editable because they are visible.
  const credentialState = {
    draft: "",
    preview: "",
    loadedRaw: "",
    loadedKind: "none",
    cleared: false,
    reveal: false,
  };
  let credentialFieldTomlLocked = false;
  const credentialClassHint = $("#provider-credential-class");

  // Editing a loaded env name into a truncation of that name
  // (OPENAI_API_KEY → OPENAI or OPENAIAPIKEY) must not become an inline
  // secret. Unrelated tokens such as AKIA… are replacements, not truncations.
  // Keep in lockstep with is_truncated_env_name in src/webui.rs.
  function compactEnvName(value) {
    return String(value || "").replaceAll("_", "");
  }

  function isTruncatedEnvName(loaded, draft) {
    if (!draft || looksLikeEnvVarName(draft)) return false;
    const loadedCompact = compactEnvName(loaded);
    const draftCompact = compactEnvName(draft);
    return !!draftCompact && loadedCompact.startsWith(draftCompact);
  }

  function isAmbiguousEnvReplacement(draft) {
    return credentialState.loadedKind === "env"
      && isTruncatedEnvName(credentialState.loadedRaw, draft);
  }

  function isInlineKeyLocked() {
    return credentialState.loadedKind === "inline"
      && !credentialState.cleared
      && !!credentialState.preview
      && !String(credentialState.draft || "").trim();
  }

  function looksLikeEnvVarDraft(draft) {
    const trimmed = String(draft || "").trim();
    if (!trimmed) {
      return credentialState.loadedKind === "env" && !credentialState.cleared;
    }
    if (looksLikeEnvVarName(trimmed)) {
      return true;
    }
    return /^[A-Z_][A-Z0-9_]*$/.test(trimmed);
  }

  // Keep in lockstep with looks_like_masked_api_key_preview in src/webui.rs.
  function looksLikeMaskedApiKeyPreview(value) {
    if (!value || !value.includes("•")) {
      return false;
    }
    if ([...value].every((ch) => ch === "•")) {
      return true;
    }
    return value.includes("••");
  }

  function credentialInputType() {
    if (isInlineKeyLocked()) {
      return "text";
    }
    return looksLikeEnvVarDraft(credentialState.draft) ? "text" : "password";
  }

  function applyCredentialFieldAccess() {
    const locked = credentialFieldTomlLocked || isInlineKeyLocked();
    apiKeyInput.readOnly = locked;
    apiKeyInput.type = credentialInputType();
    if (credentialFieldTomlLocked) {
      apiKeyInput.title = "TOML-backed providers manage credentials in TOML.";
    } else if (isInlineKeyLocked()) {
      apiKeyInput.title = "Saved API key is masked. Use Clear saved credentials to remove or replace it.";
    } else {
      apiKeyInput.title = "";
    }
  }

  function updateCredentialClassHint() {
    if (!credentialClassHint) return;
    const draft = String(credentialState.draft || "").trim();
    let text = "";
    if (credentialFieldTomlLocked) {
      text = "Credentials for this provider are owned by TOML.";
    } else if (isInlineKeyLocked()) {
      text = "Saved API key (masked). Leave it unchanged to keep it, or use Clear saved credentials to remove or replace it.";
    } else if (!draft) {
      if (credentialState.cleared || credentialState.loadedKind === "env") {
        text = "Empty field removes stored credentials on save.";
      } else if (credentialState.loadedKind === "inline") {
        text = "Leave empty to keep the saved API key.";
      } else {
        text = "Uppercase names with an underscore are stored as environment variable names. Other values are stored as API keys.";
      }
    } else if (looksLikeEnvVarName(draft)) {
      text = "Will be stored as an environment variable name and read from the process environment. It is not stored as a secret.";
    } else if (isAmbiguousEnvReplacement(draft)) {
      text = "This looks like a shortened environment variable name. Enter a full NAME_WITH_UNDERSCORE or paste an API key such as sk-….";
    } else {
      text = "Will be stored as an API key. After save, only a short prefix and suffix are shown.";
    }
    credentialClassHint.textContent = text;
    credentialClassHint.hidden = !text;
  }

  function isUnchangedLoadedEnvName(draft) {
    return credentialState.loadedKind === "env"
      && !credentialState.cleared
      && !!draft
      && draft === String(credentialState.loadedRaw || "").trim();
  }

  function credentialPatch() {
    const draft = String(credentialState.draft || "").trim();
    if (credentialFieldTomlLocked) {
      return { kind: "keep" };
    }
    if (isInlineKeyLocked()) {
      return { kind: "keep" };
    }
    if (isUnchangedLoadedEnvName(draft)) {
      return { kind: "keep" };
    }
    if (draft && credentialState.preview && draft === credentialState.preview) {
      return { kind: "keep" };
    }
    if (looksLikeMaskedApiKeyPreview(draft)) {
      return {
        kind: "invalid",
        message: "That looks like a masked preview, not an API key. Paste the full secret or an environment variable name.",
      };
    }
    if (draft) {
      if (isAmbiguousEnvReplacement(draft)) {
        return {
          kind: "invalid",
          message: "That value looks like a shortened environment variable name, not a new API key. Enter a full NAME_WITH_UNDERSCORE or paste an API key such as sk-….",
        };
      }
      return { kind: "set", value: draft };
    }
    if (credentialState.cleared || credentialState.loadedKind === "env") {
      return { kind: "clear" };
    }
    if (credentialState.loadedKind === "inline" || credentialState.preview) {
      return { kind: "keep" };
    }
    return { kind: "set", value: null };
  }

  function renderCredentialInput() {
    applyCredentialFieldAccess();
    const draft = credentialState.draft || "";
    const preview = credentialState.preview || "";
    if (isInlineKeyLocked()) {
      apiKeyInput.value = preview;
      updateCredentialClassHint();
      return;
    }
    if (draft) {
      apiKeyInput.value = (!credentialState.reveal && !looksLikeEnvVarName(draft))
        ? maskApiKey(draft)
        : draft;
      updateCredentialClassHint();
      return;
    }
    apiKeyInput.value = "";
    updateCredentialClassHint();
  }

  function setCredentialInput(raw, preview = "", saved = false, inlineSaved = false) {
    const trimmed = String(raw || "").trim();
    credentialState.draft = trimmed;
    credentialState.preview = preview || "";
    credentialState.cleared = false;
    credentialState.reveal = false;
    if (!saved) {
      credentialState.loadedRaw = "";
      credentialState.loadedKind = "none";
    } else if (looksLikeEnvVarName(trimmed)) {
      credentialState.loadedRaw = trimmed;
      credentialState.loadedKind = "env";
    } else if (preview || inlineSaved) {
      credentialState.loadedRaw = "";
      credentialState.loadedKind = "inline";
    } else {
      credentialState.loadedRaw = "";
      credentialState.loadedKind = "none";
    }
    renderCredentialInput();
  }

  function setClearCredentialsVisible(visible) {
    if (clearCredentialsBtn) {
      clearCredentialsBtn.hidden = !visible;
    }
  }

  if (clearCredentialsBtn) {
    clearCredentialsBtn.addEventListener("click", () => {
      credentialState.draft = "";
      credentialState.preview = "";
      credentialState.cleared = true;
      credentialState.reveal = false;
      renderCredentialInput();
      apiKeyInput.focus();
    });
  }
  apiKeyInput.addEventListener("focus", () => {
    if (apiKeyInput.readOnly) return;
    const draft = credentialState.draft || "";
    if (draft) {
      credentialState.reveal = true;
      apiKeyInput.type = credentialInputType();
      apiKeyInput.value = draft;
    }
  });
  apiKeyInput.addEventListener("input", () => {
    if (apiKeyInput.readOnly) return;
    credentialState.draft = apiKeyInput.value;
    credentialState.reveal = true;
    applyCredentialFieldAccess();
    updateCredentialClassHint();
  });
  function syncEditableCredentialFromInput() {
    if (apiKeyInput.readOnly) return;
    const visible = String(apiKeyInput.value || "");
    if (!visible || looksLikeMaskedApiKeyPreview(visible)) return;
    credentialState.draft = visible;
  }

  apiKeyInput.addEventListener("blur", () => {
    syncEditableCredentialFromInput();
    credentialState.reveal = false;
    if (credentialState.draft) {
      credentialState.draft = String(credentialState.draft).trim();
    }
    renderCredentialInput();
  });

  $("#btn-add-provider").addEventListener("click", () => {
    void openProviderForm();
  });
  $("#provider-form-cancel").addEventListener("click", () => providerDialog.close());
  templateSelect.addEventListener("change", () => applySelectedTemplate());
  addProviderHeaderBtn.addEventListener("click", () => addProviderHeaderRow());

  providerForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(providerForm);
    const id = String(fd.get("id") || "").trim();
    const mode = providerForm.dataset.mode || "create";
    const template = mode === "create"
      ? findTemplateByOptionValue(templateSelect.value)
      : null;
    syncEditableCredentialFromInput();
    const credential = credentialPatch();
    if (credential.kind === "invalid") {
      status(credential.message, { isError: true });
      return;
    }
    const body = {
      name: String(fd.get("name") || "").trim() || null,
      base_url: String(fd.get("base_url") || "").trim(),
      ...(credential.kind === "keep"
        ? {}
        : credential.kind === "clear"
          ? { api_key_env: null, api_key: null }
          : { api_key_env: credential.value }),
      auth_header: String(fd.get("auth_header") || "").trim() || "authorization",
      auth_scheme: String(fd.get("auth_scheme") ?? "Bearer").trim(),
      responses_path: String(fd.get("responses_path") || "").trim() || "/responses",
      chat_completions_path:
        String(fd.get("chat_completions_path") || "").trim() || "/chat/completions",
      models_path: String(fd.get("models_path") || "").trim() || "/models",
      model_catalog_only: providerForm.querySelector("[name=model_catalog_only]").checked,
      request_stream_options_include_usage:
        providerForm.querySelector("[name=request_stream_options_include_usage]").checked,
      enabled: providerForm.querySelector("[name=enabled]")?.checked ?? true,
    };
    try {
      const headers = collectProviderHeadersFromForm(mode);
      if (mode === "create") {
        const isCustom = !template || template.key === "custom";
        const payload = isCustom
          ? {
              template: "custom",
              ...(id ? { id } : {}),
              ...body,
              ...(headers ? { headers } : {}),
              model_catalog: selectedTemplateCatalog,
            }
          : {
              template: template.key,
              id: template.id,
              name: body.name,
              ...(Object.hasOwn(body, "api_key_env") ? { api_key_env: body.api_key_env } : {}),
              ...(Object.hasOwn(body, "api_key") ? { api_key: body.api_key } : {}),
              request_stream_options_include_usage: body.request_stream_options_include_usage,
              enabled: body.enabled,
              ...(headers ? { headers } : {}),
            };
        const created = await api("/providers", {
          method: "POST",
          body: JSON.stringify(payload),
        });
        providerDialog.close();
        await loadProviders({ refreshRoutes: false });
        status(`Provider ${created.id} created`);
        return;
      } else {
        const targetId = id;
        await api(`/providers/${encodeURIComponent(targetId)}`, {
          method: "PUT",
          body: JSON.stringify({
            name: body.name,
            base_url: body.base_url,
            ...(Object.hasOwn(body, "api_key_env") ? { api_key_env: body.api_key_env } : {}),
            ...(Object.hasOwn(body, "api_key") ? { api_key: body.api_key } : {}),
            auth_header: body.auth_header,
            auth_scheme: body.auth_scheme,
            responses_path: body.responses_path,
            chat_completions_path: body.chat_completions_path,
            models_path: body.models_path,
            ...(headers ? { headers } : {}),
            model_catalog_only: body.model_catalog_only,
            request_stream_options_include_usage: body.request_stream_options_include_usage,
            enabled: body.enabled,
          }),
        });
        providerDialog.close();
        await loadProviders({ refreshRoutes: false });
        status(`Provider ${targetId} updated`);
      }
    } catch (e) { status(`Error: ${formatErrorMessage(e)}`); }
  });

  function addProviderHeaderRow(name = "", value = "") {
    const row = document.createElement("div");
    row.className = "provider-header-row";
    const headerName = document.createElement("label");
    headerName.textContent = "Header";
    const nameInput = document.createElement("input");
    nameInput.name = "provider-header-name";
    nameInput.placeholder = "X-Header";
    nameInput.value = name;
    headerName.append(nameInput);

    const headerValue = document.createElement("label");
    headerValue.textContent = "Value";
    const valueInput = document.createElement("input");
    valueInput.name = "provider-header-value";
    valueInput.placeholder = "value";
    valueInput.value = value;
    headerValue.append(valueInput);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "btn small danger";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => {
      row.remove();
      if (!providerHeadersRows.children.length) {
        addProviderHeaderRow();
      }
    });
    row.append(headerName, headerValue, remove);
    providerHeadersRows.append(row);
  }

  function collectProviderHeadersFromForm(mode = "create") {
    const rows = Array.from(
      providerHeadersRows.querySelectorAll(".provider-header-row"),
    );
    const headers = {};
    const seen = Object.create(null);
    for (const row of rows) {
      const rawName = row.querySelector("[name=provider-header-name]");
      const rawValue = row.querySelector("[name=provider-header-value]");
      if (!rawName || !rawValue) {
        continue;
      }
      const key = String(rawName.value || "").trim();
      if (!key) {
        continue;
      }
      const folded = asciiHeaderNameKey(key);
      if (Object.hasOwn(seen, folded)) {
        throw new Error(`Duplicate custom header "${key}"`);
      }
      seen[folded] = key;
      headers[key] = String(rawValue.value || "");
    }
    if (Object.keys(headers).length) {
      return headers;
    }
    // Custom edit with only blank rows is an explicit clear. Named/template
    // edits hide the section and must omit headers to preserve TOML values.
    if (mode === "edit" && !providerHeadersSection.hidden) {
      return {};
    }
    return null;
  }

  function asciiHeaderNameKey(name) {
    return String(name).replace(/[A-Z]/g, (ch) => String.fromCharCode(ch.charCodeAt(0) + 32));
  }

  function applyProviderHeaders(headers = null) {
    providerHeadersRows.innerHTML = "";
    if (!headers) {
      return;
    }
    const entries = Object.entries(headers);
    if (!entries.length) {
      return;
    }
    for (const [name, value] of entries) {
      addProviderHeaderRow(name, value);
    }
  }

  function setCustomHeadersMode(isCustom) {
    providerHeadersSection.hidden = !isCustom;
    if (!isCustom) {
      providerHeadersRows.innerHTML = "";
      return;
    }
    if (!providerHeadersRows.children.length) {
      addProviderHeaderRow();
    }
  }

  function templateOptionValue(template) {
    return template && template.key ? template.key : "";
  }

  function findTemplateByOptionValue(value) {
    return providerTemplates.find((template) => template.key === value) || null;
  }

  function populateTemplateSelect() {
    templateSelect.innerHTML = "";
    providerTemplates.forEach((template) => {
      const option = document.createElement("option");
      option.value = templateOptionValue(template);
      option.textContent = template.label;
      templateSelect.append(option);
    });
  }

  function renderCatalogPreview(catalog) {
    if (!catalog?.length) {
      templateCatalogPreview.hidden = true;
      templateCatalogPreview.innerHTML = "";
      return;
    }
    const names = catalog
      .map((entry) => entry.display_name || entry.id)
      .slice(0, 8);
    templateCatalogPreview.hidden = false;
    const summary = document.createElement("strong");
    summary.textContent = `${catalog.length} catalog model${catalog.length === 1 ? "" : "s"}`;
    const namesDiv = document.createElement("div");
    namesDiv.textContent =
      names.join(" · ") + (catalog.length > 8 ? ` +${catalog.length - 8} more` : "");
    templateCatalogPreview.replaceChildren(summary, namesDiv);
  }

  function setNamedTemplateMode(isNamed) {
    const identity = $("#provider-identity-fields");
    const advanced = $("#provider-advanced");
    const idInput = providerForm.querySelector("[name=id]");
    const baseUrlInput = providerForm.querySelector("[name=base_url]");
    const nameInput = providerForm.querySelector("[name=name]");
    identity.classList.remove("template-locked");
    advanced.hidden = isNamed;
    baseUrlInput.readOnly = isNamed;
    baseUrlInput.classList.toggle("template-locked", isNamed);
    nameInput.readOnly = false;
    ["auth_header", "auth_scheme", "responses_path", "chat_completions_path", "models_path"]
      .forEach((name) => {
        providerForm.querySelector(`[name=${name}]`).readOnly = isNamed;
      });
    providerForm.querySelector("[name=model_catalog_only]").disabled = isNamed;
    idInput.readOnly = true;
  }

  function applySelectedTemplate() {
    const template = findTemplateByOptionValue(templateSelect.value);
    const idInput = providerForm.querySelector("[name=id]");
    if (!template) {
      selectedTemplateCatalog = [];
      templateDescription.textContent = "";
      renderCatalogPreview([]);
      setNamedTemplateMode(false);
      setCustomHeadersMode(true);
      return;
    }
    const isNamed = template.key !== "custom";
    selectedTemplateCatalog = Array.isArray(template.model_catalog)
      ? template.model_catalog.map((entry) => ({ ...entry }))
      : [];
    templateDescription.textContent = template.description || "";
    idInput.value = template.id || "";
    providerForm.querySelector("[name=name]").value = template.name || "";
    providerForm.querySelector("[name=base_url]").value = template.base_url || "";
    setCredentialInput(
      template.api_key_env || "",
      "",
      looksLikeEnvVarName(template.api_key_env || ""),
    );
    providerForm.querySelector("[name=auth_header]").value =
      template.auth_header || "authorization";
    providerForm.querySelector("[name=auth_scheme]").value = template.auth_scheme ?? "Bearer";
    providerForm.querySelector("[name=responses_path]").value =
      template.responses_path || "/responses";
    providerForm.querySelector("[name=chat_completions_path]").value =
      template.chat_completions_path || "/chat/completions";
    providerForm.querySelector("[name=models_path]").value = template.models_path || "/models";
    providerForm.querySelector("[name=model_catalog_only]").checked = !!template.model_catalog_only;
    providerForm.querySelector("[name=enabled]").checked = true;
    applyProviderHeaders(template.headers ?? null);
    setNamedTemplateMode(isNamed);
    setCustomHeadersMode(true);
    renderCatalogPreview(selectedTemplateCatalog);
    providerForm.querySelector("[name=api_key_env]").focus();
  }

  function findTemplateForProvider(provider) {
    if (!provider) return null;
    return (
      providerTemplates.find((template) => template.key === provider.template_key) ||
      providerTemplates.find((template) => template.id === provider.id) ||
      providerTemplates.find((template) => template.key === "custom")
    );
  }

  async function ensureProviderTemplates() {
    if (providerTemplates.length) return;
    await loadProviderTemplates();
    if (!providerTemplates.length) {
      throw new Error("Provider templates are not available yet.");
    }
  }

  async function openProviderForm(p = null) {
    try {
      await ensureProviderTemplates();
    } catch (e) {
      status(`Error: ${formatErrorMessage(e)}`);
      // Create needs a template catalog. Edit can still open: a missing
      // match must be treated as custom, not as a named template.
      if (!p) return;
    }
    selectedTemplateCatalog = [];
    const idInput = providerForm.querySelector("[name=id]");
    const enabledField = $("#provider-enabled-field");
    if (p) {
      providerForm.dataset.mode = "edit";
      populateTemplateSelect();
      $("#provider-form-title").textContent = "Edit provider";
      templateField.hidden = false;
      templateSelect.disabled = true;
      const matching = findTemplateForProvider(p);
      const isNamed = !!p.named_template;
      const allowCustomHeaders = !!p.managed;
      const selectedOption = matching
        ? templateOptionValue(matching)
        : templateOptionValue(
            providerTemplates.find((template) => template.key === "custom") ||
              providerTemplates[0],
          );
      if (selectedOption) {
        templateSelect.value = selectedOption;
      }
      templateDescription.textContent =
        matching?.description ||
        (p.named_template
          ? "This provider was created from a bundled example template."
          : "This provider does not match a bundled example template.");
      templateCatalogPreview.hidden = true;
      enabledField.hidden = false;
      idInput.value = p.id;
      idInput.readOnly = true;
      providerForm.querySelector("[name=name]").value = p.name || "";
      providerForm.querySelector("[name=base_url]").value = p.base_url || "";
      credentialFieldTomlLocked = !p.managed;
      setCredentialInput(
        p.api_key_env || "",
        p.managed ? (p.api_key_preview || "") : "",
        true,
        !!(p.managed && p.has_inline_api_key && !p.api_key_env),
      );
      providerForm.querySelector("[name=auth_header]").value = p.auth_header || "authorization";
      providerForm.querySelector("[name=auth_scheme]").value = p.auth_scheme ?? "Bearer";
      providerForm.querySelector("[name=responses_path]").value = p.responses_path || "/responses";
      providerForm.querySelector("[name=chat_completions_path]").value =
        p.chat_completions_path || "/chat/completions";
      providerForm.querySelector("[name=models_path]").value = p.models_path || "/models";
      providerForm.querySelector("[name=model_catalog_only]").checked = !!p.model_catalog_only;
      providerForm.querySelector("[name=request_stream_options_include_usage]").checked =
        !!p.request_stream_options_include_usage;
      providerForm.querySelector("[name=enabled]").checked = !!p.enabled;
      if (allowCustomHeaders) {
        applyProviderHeaders(p.headers);
      } else {
        applyProviderHeaders(null);
      }
      setNamedTemplateMode(isNamed);
      setCustomHeadersMode(allowCustomHeaders);
      $("#provider-advanced").hidden = false;
      apiKeyInput.placeholder = p.api_key_env
        ? "PROVIDER_API_KEY"
        : (p.has_inline_api_key ? "Saved API key" : "PROVIDER_API_KEY or sk-…");
      setClearCredentialsVisible(
        p.managed && !!(p.api_key_env || p.has_inline_api_key),
      );
    } else {
      providerForm.reset();
      providerIdInput.value = "";
      applyProviderHeaders(null);
      credentialFieldTomlLocked = false;
      apiKeyInput.placeholder = "PROVIDER_API_KEY or sk-…";
      setCredentialInput("");
      setClearCredentialsVisible(false);
      providerForm.dataset.mode = "create";
      $("#provider-form-title").textContent = "Add provider";
      templateField.hidden = false;
      templateSelect.disabled = false;
      enabledField.hidden = false;
      populateTemplateSelect();
      const preferred =
        providerTemplates.find((template) => template.key === "custom") ||
        providerTemplates[0];
      if (preferred) {
        templateSelect.value = templateOptionValue(preferred);
      }
      applySelectedTemplate();
    }
    providerDialog.showModal();
  }

  async function loadProviderTemplates() {
    providerTemplates = await api("/provider-templates");
    populateTemplateSelect();
  }

  const modelDialog = $("#model-dialog");
  const modelForm = $("#model-form");
  let editingModel = null;
  function parseReasoningLevels(value) {
    return [...new Set(String(value || "").split(",").map((level) => level.trim()).filter(Boolean))];
  }

  function sameStringArray(left, right) {
    return left.length === right.length && left.every((value, index) => value === right[index]);
  }

  $("#model-form-cancel").addEventListener("click", () => modelDialog.close());
  modelForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(modelForm);
    const providerId = fd.get("provider_id");
    const upstreamId = fd.get("upstream_id")?.trim() || "";
    const levelsText = fd.get("supported_reasoning_levels")?.trim() || "";
    const levels = parseReasoningLevels(levelsText);
    const defaultLevel = fd.get("default_reasoning_level")?.trim() || "";
    const mode = modelForm.dataset.mode || "create";
    let id;
    if (mode === "create") {
      if (!upstreamId) {
        status("Model ID is required");
        return;
      }
      id = `${providerId}/${upstreamId}`.toLowerCase();
    } else {
      id = fd.get("id")?.trim() || "";
    }
    if (defaultLevel && levelsText && !levels.includes(defaultLevel)) {
      status("Default reasoning mode must be included in the available modes");
      return;
    }
    const body = {
      id,
      upstream_id: upstreamId || null,
      enabled: editingModel?.enabled ?? true,
    };
    const displayName = fd.get("display_name")?.trim() || "";
    const description = fd.get("description")?.trim() || "";
    if (!editingModel) {
      if (displayName) body.display_name = displayName;
      if (description) body.description = description;
    } else {
      if (displayName !== (editingModel.display_name || "")) body.display_name = displayName || null;
      if (description !== (editingModel.description || "")) body.description = description || null;
    }
    const previousLevels = Array.isArray(editingModel?.supported_reasoning_levels)
      ? editingModel.supported_reasoning_levels
      : [];
    const previousDefault = editingModel?.default_reasoning_level || "";
    const levelsChanged = !!editingModel && !sameStringArray(levels, previousLevels);
    const defaultChanged = !!editingModel && defaultLevel !== previousDefault;
    if (!editingModel) {
      if (levelsText) body.supported_reasoning_levels = levels;
      if (defaultLevel) body.default_reasoning_level = defaultLevel;
    } else {
      if (levelsChanged) body.supported_reasoning_levels = levelsText ? levels : null;
      if (defaultChanged) body.default_reasoning_level = defaultLevel || null;
    }
    try {
      if (mode === "create" || mode === "promote") {
        await api(`/providers/${encodeURIComponent(providerId)}/models`, {
          method: "POST",
          body: JSON.stringify(body),
        });
      } else {
        await api(
          `/providers/${encodeURIComponent(providerId)}/models/${encodeURIComponent(id)}`,
          { method: "PUT", body: JSON.stringify(body) },
        );
      }
      modelDialog.close();
      await loadProviders({ refreshRoutes: false });
    } catch (e) { status(`Error: ${formatErrorMessage(e)}`); }
  });

  function openModelForm(providerId, m = null) {
    modelForm.reset();
    editingModel = m;
    modelForm.querySelector("[name=provider_id]").value = providerId;
    const idInput = modelForm.querySelector("[name=id]");
    if (m) {
      modelForm.dataset.mode = m.catalog ? "edit" : "promote";
      $("#model-form-title").textContent = "Edit model";
      idInput.value = m.id;
      modelForm.querySelector("[name=upstream_id]").value = m.upstream_id || (m.catalog ? "" : m.id);
      modelForm.querySelector("[name=display_name]").value = m.display_name || "";
      modelForm.querySelector("[name=description]").value = m.description || "";
      modelForm.querySelector("[name=supported_reasoning_levels]").value =
        (m.supported_reasoning_levels || []).join(", ");
      modelForm.querySelector("[name=default_reasoning_level]").value =
        m.default_reasoning_level || "";
    } else {
      modelForm.dataset.mode = "create";
      $("#model-form-title").textContent = "Add model";
      idInput.value = "";
    }
    modelDialog.showModal();
  }

  function fillAnalyticsFilters() {
    const provSel = $("#analytics-provider");
    const cur = provSel.value;
    provSel.innerHTML = "<option value=''>All providers</option>";
    for (const p of providers) {
      const o = document.createElement("option");
      o.value = p.id;
      o.textContent = p.display_name || p.id;
      provSel.append(o);
    }
    for (const id of analyticsProviderIds) {
      if (![...provSel.options].some((option) => option.value === id)) {
        const o = document.createElement("option");
        o.value = id;
        o.textContent = id;
        provSel.append(o);
      }
    }
    provSel.value = cur;
    const modelSel = $("#analytics-model");
    const mcur = modelSel.value;
    modelSel.innerHTML = "<option value=''>All models</option>";
    const seen = new Set();
    const prov = providers.find((p) => p.id === provSel.value);
    if (prov) {
      for (const m of prov.models || []) {
        seen.add(m.id);
        const o = document.createElement("option");
        o.value = m.id;
        o.textContent = m.display_name || m.id;
        modelSel.append(o);
      }
    }
    const inventoryMatches = analyticsModelProvider === provSel.value;
    for (const id of inventoryMatches ? analyticsModelIds : []) {
      if (!seen.has(id)) {
        seen.add(id);
        const o = document.createElement("option");
        o.value = id;
        o.textContent = id;
        modelSel.append(o);
      }
    }
    modelSel.value = [...modelSel.options].some((o) => o.value === mcur) ? mcur : "";
  }

  function analyticsOptionValue(select, saved) {
    if (typeof saved !== "string") return null;
    return [...select.options].some((option) => option.value === saved) ? saved : null;
  }

  function analyticsFiltersSnapshot() {
    return {
      version: ANALYTICS_FILTERS_VERSION,
      range: $("#analytics-range").value,
      provider: $("#analytics-provider").value,
      model: $("#analytics-model").value,
    };
  }

  function writeAnalyticsFilters(filters = analyticsFiltersSnapshot()) {
    try {
      sessionStorage.setItem(ANALYTICS_FILTERS_KEY, JSON.stringify(filters));
    } catch {
      /* optional persistence */
    }
  }

  function storeAnalyticsFilters(changedFilter = null) {
    let filters = analyticsFiltersSnapshot();
    if (changedFilter === "range" &&
        analyticsFiltersToRestore?.version === ANALYTICS_FILTERS_VERSION) {
      // Range is available synchronously, while provider/model options can be
      // waiting on their normal option rebuilds. Changing only range must not
      // reject those untouched pending selections.
      filters = {
        ...analyticsFiltersToRestore,
        version: ANALYTICS_FILTERS_VERSION,
        range: filters.range,
      };
      analyticsFiltersToRestore = filters;
      writeAnalyticsFilters(filters);
      return;
    }
    // Provider/model changes are authoritative user choices. A provider
    // change also deliberately resets its dependent model before this call.
    analyticsFiltersToRestore = null;
    writeAnalyticsFilters(filters);
  }

  function restoreAnalyticsFilters() {
    const saved = analyticsFiltersToRestore;
    if (!saved) return false;
    const range = $("#analytics-range");
    const provider = $("#analytics-provider");
    const model = $("#analytics-model");
    const before = [range.value, provider.value, model.value];
    fillAnalyticsFilters();
    range.value = analyticsOptionValue(range, saved.range) ?? range.value;
    const savedProvider = analyticsOptionValue(provider, saved.provider);
    if (savedProvider !== null) provider.value = savedProvider;
    // Provider selection controls the valid model inventory, so rebuild it
    // before validating the stored model value.
    fillAnalyticsFilters();
    const providerMatches = savedProvider !== null;
    const savedModel = providerMatches
      ? analyticsOptionValue(model, saved.model)
      : null;
    if (savedModel !== null) model.value = savedModel;
    if (analyticsOptionValue(range, saved.range) !== null &&
        savedProvider !== null &&
        savedModel !== null) {
      analyticsFiltersToRestore = null;
      writeAnalyticsFilters();
    }
    const after = [range.value, provider.value, model.value];
    return before.some((value, index) => value !== after[index]);
  }

  function reconcileAnalyticsFiltersAfterInventory() {
    fillAnalyticsFilters();
    return restoreAnalyticsFilters();
  }

  let analyticsPending = { queued: false, fromPoll: true };

  function refreshRestoredAnalytics(restoredFilters) {
    if (restoredFilters && bootComplete && activeTab === "analytics") {
      void loadAnalytics({ fromPoll: true });
    }
  }

  function requestAnalytics(changedFilter) {
    storeAnalyticsFilters(changedFilter);
    void loadAnalytics({ fromPoll: false });
  }

  $("#analytics-provider").addEventListener("change", () => {
    $("#analytics-model").value = "";
    fillAnalyticsFilters();
    requestAnalytics("provider");
  });
  $("#analytics-range").addEventListener("change", () => requestAnalytics("range"));
  $("#analytics-model").addEventListener("change", () => requestAnalytics("model"));

  async function loadAnalytics({ fromPoll = false } = {}) {
    if (analyticsInFlight) {
      analyticsPending.fromPoll = analyticsPending.queued
        ? analyticsPending.fromPoll && fromPoll
        : fromPoll;
      analyticsPending.queued = true;
      return;
    }
    analyticsInFlight = true;
    const reportFromPoll = fromPoll;
    analyticsPending.queued = false;
    analyticsPending.fromPoll = true;
    const range = $("#analytics-range").value;
    const provider = $("#analytics-provider").value;
    const model = $("#analytics-model").value;
    // One comparison for success and catch: a stale in-flight response must
    // neither paint nor replace a newer request's error with this one's.
    const analyticsFiltersChanged = () =>
      $("#analytics-range").value !== range ||
      $("#analytics-provider").value !== provider ||
      $("#analytics-model").value !== model;
    const qs = new URLSearchParams({ range });
    if (provider) qs.set("provider", provider);
    if (model) qs.set("model", model);
    try {
      const data = await api(`/analytics?${qs}`);
      // Filters can change while this request is in flight (queued follow-up
      // runs in `finally`). Applying this payload would paint the old window
      // against the new dropdowns until that follow-up returns.
      if (analyticsFiltersChanged()) {
        return;
      }
      // Filtered responses omit the selected dimension's breakdown. Preserve
      // that option inventory until the user selects a different scope.
      if (!provider) {
        analyticsProviderIds = (data.by_provider || [])
          .map((row) => row.key)
          .filter(Boolean);
      }
      if (!model) {
        analyticsModelIds = (data.by_model || [])
          .map((row) => row.key)
          .filter(Boolean);
        analyticsModelProvider = provider;
      }
      if (reconcileAnalyticsFiltersAfterInventory()) {
        // A newly available saved option changed the effective filters. Fetch
        // again instead of painting this response under different controls.
        analyticsPending.queued = true;
        analyticsPending.fromPoll = reportFromPoll;
        return;
      }
      analyticsSnapshot = {
        data,
        range,
        provider,
        model,
        barTitle: model
          ? `${model} over time`
          : provider
            ? `${provider} over time`
            : "Usage over time",
      };
      renderAnalyticsPresentation();
      if (activeTab === "analytics") {
        const message = "Analytics updated";
        if (reportFromPoll) pollStatus(message);
        else status(message);
      }
    } catch (e) {
      if (analyticsFiltersChanged()) {
        return;
      }
      if (activeTab === "analytics") {
        const message = `Analytics error: ${formatErrorMessage(e)}`;
        if (reportFromPoll) pollStatus(message, { isError: true });
        else status(message, { isError: true });
      }
    } finally {
      analyticsInFlight = false;
      if (analyticsPending.queued && activeTab === "analytics") {
        const queuedFromPoll = analyticsPending.fromPoll;
        analyticsPending.queued = false;
        loadAnalytics({ fromPoll: queuedFromPoll });
      }
    }
  }

  // Chart math is a sibling deferred script. Missing it must not abort this
  // IIFE — providers, logs, and analytics cards still have to boot.
  const Charts = globalThis.CodexWarpCharts || null;
  function applyChartCanvasAttrs(canvas, attrs) {
    if (!canvas.dataset.labelledby) {
      const labelled = canvas.getAttribute("aria-labelledby");
      if (labelled) canvas.dataset.labelledby = labelled;
    }
    if (attrs.tabIndex == null) canvas.removeAttribute("tabindex");
    else canvas.setAttribute("tabindex", String(attrs.tabIndex));
    if (attrs.role) canvas.setAttribute("role", attrs.role);
    else canvas.removeAttribute("role");
    if (attrs.keyshortcuts) canvas.setAttribute("aria-keyshortcuts", attrs.keyshortcuts);
    else canvas.removeAttribute("aria-keyshortcuts");
    if (attrs.describedBy) canvas.setAttribute("aria-describedby", attrs.describedBy);
    else canvas.removeAttribute("aria-describedby");
    if (attrs.labelledBy && canvas.dataset.labelledby) {
      canvas.setAttribute("aria-labelledby", canvas.dataset.labelledby);
    } else {
      canvas.removeAttribute("aria-labelledby");
    }
    if (attrs.ariaHidden) canvas.setAttribute("aria-hidden", "true");
    else canvas.removeAttribute("aria-hidden");
    if (attrs.tabIndex == null && document.activeElement === canvas) canvas.blur();
  }
  function applyChartChrome(attrs) {
    const kbdHelp = $("#chart-kbd-help");
    if (kbdHelp) kbdHelp.hidden = !!attrs.kbdHelpHidden;
    document.querySelectorAll(".chart-fallback").forEach((el) => {
      el.hidden = !!attrs.fallbackHidden;
    });
  }
  function applyChartInteractivity(surface) {
    const attrs = Charts
      ? Charts.chartCanvasAttrs(surface)
      : {
          tabIndex: null,
          role: null,
          keyshortcuts: null,
          describedBy: null,
          labelledBy: null,
          ariaHidden: true,
          kbdHelpHidden: true,
          fallbackHidden: false,
        };
    document.querySelectorAll(".chart-wrap canvas").forEach((canvas) => {
      applyChartCanvasAttrs(canvas, attrs);
    });
    applyChartChrome(attrs);
  }
  function chartsLiveLayout() {
    const canvas = $("#chart-line") || $("#chart-bar");
    if (!canvas || !Charts) return false;
    return Charts.chartsLiveLayout(canvas.clientWidth);
  }
  function syncChartSurface() {
    if (!Charts) {
      noteChartsUnavailable();
      return;
    }
    const live = chartsLiveLayout();
    let anyInteractive = false;
    document.querySelectorAll(".chart-wrap canvas").forEach((canvas) => {
      const count = Charts.chartNavigableCount
        ? Charts.chartNavigableCount(canvas.__chart)
        : 0;
      const surface = Charts.chartSurface(true, count, live);
      applyChartCanvasAttrs(canvas, Charts.chartCanvasAttrs(surface));
      if (surface === "interactive") anyInteractive = true;
    });
    applyChartChrome(Charts.chartCanvasAttrs(anyInteractive ? "interactive" : "idle"));
  }
  function noteChartsUnavailable() {
    applyChartInteractivity("failed");
  }
  if (!Charts) noteChartsUnavailable();
  function formatBucketLabel(ms, style) {
    return Charts ? Charts.formatBucketLabel(ms, style) : "";
  }

  function labelStyleFor(points) {
    return Charts ? Charts.bucketLabelStyle(points) : "time";
  }

  function renderAnalyticsPresentation() {
    if (!analyticsSnapshot) return;
    const { data, range, barTitle } = analyticsSnapshot;
    renderAnalyticsCards(data);
    $("#chart-bar-title").textContent = barTitle;
    if (!Charts) {
      noteChartsUnavailable();
      return;
    }
    const series = data.series || [];
    if (!chartsLiveLayout()) {
      syncChartSurface();
      if (activeTab === "analytics") scheduleChartResize();
      return;
    }
    const labelStyle = labelStyleFor(series);
    drawLineChart($("#chart-line"), series, range);
    // Bar chart shows the same time series as bars so usage-over-time is visible
    // in both chart styles; breakdowns remain available via provider/model filters.
    drawBarChart(
      $("#chart-bar"),
      series.map((point) => ({
        key: formatBucketLabel(point.ts, labelStyle),
        ts: point.ts,
        total_tokens: point.total_tokens || 0,
        input_tokens: point.input_tokens || 0,
        output_tokens: point.output_tokens || 0,
        cached_tokens: point.cached_tokens || 0,
        prompts: point.prompts || 0,
        sessions: point.sessions || 0,
      })),
      range,
    );
    const modelSeries = data.model_series || [];
    if (Charts.retainPaletteKeys && Charts.paletteSlotKey) {
      const colorKeys = [];
      const pushSlot = (kind, key) => {
        if (key == null || key === "") return;
        colorKeys.push(Charts.paletteSlotKey(kind, key));
      };
      for (const series of modelSeries) {
        if (series) pushSlot("model", series.model);
      }
      for (const row of data.by_provider || []) {
        if (row) pushSlot("provider", row.key);
      }
      for (const group of [data.by_model_overall, data.by_model]) {
        for (const row of group || []) {
          if (row) pushSlot("model", row.key);
        }
      }
      Charts.retainPaletteKeys(identityPaletteAssigned, colorKeys);
    }
    drawModelUsageChart($("#chart-model-sessions"), modelSeries, "sessions", range);
    drawModelUsageChart($("#chart-model-prompts"), modelSeries, "prompts", range);
    drawModelUsageChart($("#chart-model-cache-rate"), modelSeries, "cache_rate", range);

    // Gate pies on the filter state the response was fetched with, not the
    // live select values: a slow poll can otherwise render a response computed
    // with different filters than the user currently sees, mislabeling the
    // per-provider pie for up to one round trip.
    const activeProvider = analyticsSnapshot.provider || "";
    const activeModel = analyticsSnapshot.model || "";
    drawPieChart($("#chart-pie-provider"), pieRows(data.by_provider, "provider"), {
      emptyText: activeProvider
        ? "Select All providers to see provider usage."
        : "No token usage in this range.",
    });
    // Overall model usage ignores the provider filter (by_model_overall), so
    // the overall pie stays global while a provider filter narrows the
    // per-provider pie below.
    drawPieChart($("#chart-pie-model"), pieRows(data.by_model_overall ?? data.by_model, "model"), {
      // The backend now fills by_model_overall even under a model filter (the
      // selected model's own window total), so an empty state means there is
      // genuinely no usage for the current filter combination.
      emptyText: "No token usage in this range.",
    });
    // Per-provider model breakdown exists only while a provider filter is
    // active and the model filter is clear (the API omits by_model otherwise).
    const perProviderRows =
      activeProvider && !activeModel ? pieRows(data.by_model, "model") : [];
    drawPieChart($("#chart-pie-provider-models"), perProviderRows, {
      emptyText: !activeProvider
        ? "Select a provider to see model usage per provider."
        : activeModel
          ? "Select All models to see model usage per provider."
          : "No token usage for this provider in this range.",
    });
    syncChartSurface();
  }

  window.addEventListener("codex-warp-theme-change", () => {
    if (activeTab === "analytics") renderAnalyticsPresentation();
  });

  function renderAnalyticsCards(d) {
    const cards = $("#analytics-cards");
    const cacheRate = Charts && Charts.cacheRatePercent
      ? Charts.cacheRatePercent(d.cached_tokens, d.input_tokens)
      : Number(d.input_tokens) > 0
        ? (Math.max(0, Number(d.cached_tokens) || 0) / Number(d.input_tokens)) * 100
        : 0;
    const cacheRateLabel = Charts && Charts.formatCacheRate
      ? Charts.formatCacheRate(cacheRate)
      : `${Math.round(cacheRate * 10) / 10}%`;
    const items = [
      ["Prompts", Number(d.prompts || 0).toLocaleString()],
      ["Sessions", Number(d.sessions || 0).toLocaleString()],
      ["Input tokens", Number(d.input_tokens || 0).toLocaleString()],
      ["Output tokens", Number(d.output_tokens || 0).toLocaleString()],
      ["Total tokens", Number(d.total_tokens || 0).toLocaleString()],
      ["Cached tokens", Number(d.cached_tokens || 0).toLocaleString()],
      ["Cache rate", cacheRateLabel],
      ["Reasoning", Number(d.reasoning_tokens || 0).toLocaleString()],
    ];
    const fragment = document.createDocumentFragment();
    for (const [label, val] of items) {
      const card = document.createElement("div");
      card.className = "card";
      const caption = document.createElement("label");
      caption.textContent = label;
      const value = document.createElement("strong");
      value.textContent = val;
      card.append(caption, value);
      fragment.append(card);
    }
    cards.replaceChildren(fragment);
  }

  const colorProbe = document.createElement("span");
  colorProbe.hidden = true;
  document.documentElement.append(colorProbe);

  function cssThemeColor(variable, fallback) {
    colorProbe.style.color = `var(${variable})`;
    const resolved = getComputedStyle(colorProbe).color;
    if (!resolved || resolved === "rgba(0, 0, 0, 0)") {
      return fallback;
    }
    return resolved;
  }

  function chartColors() {
    return {
      muted: cssThemeColor("--muted", "#71717a"),
      grid: cssThemeColor("--border", "#e4e4e7"),
      text: cssThemeColor("--text", "#18181b"),
      surface: cssThemeColor("--surface", "#ffffff"),
      tokens: cssThemeColor("--chart-tokens", "#0f766e"),
      prompts: cssThemeColor("--chart-prompts", "#d97706"),
      sessions: cssThemeColor("--chart-sessions", "#16a34a"),
      input: cssThemeColor("--chart-input", "#2563eb"),
      output: cssThemeColor("--chart-output", "#7c3aed"),
      cached: cssThemeColor("--chart-cached", "#be185d"),
      bar: cssThemeColor("--chart-tokens", "#0f766e"),
    };
  }

  // Distinct mid-tone palette shared by model-over-time lines and pie slices.
  // Values stay readable on both light and dark surfaces; slices stroke with
  // the surface color to separate neighbors.
  const CHART_PALETTE = [
    "#0f766e", "#d97706", "#2563eb", "#7c3aed", "#16a34a",
    "#dc2626", "#0891b2", "#db2777", "#65a30d", "#9333ea",
    "#ca8a04", "#0ea5e9", "#ea580c", "#14b8a6", "#e11d48",
    "#4f46e5", "#059669", "#b45309", "#0284c7", "#c026d3",
  ];
  function paletteColor(index) {
    return CHART_PALETTE[Number(index) % CHART_PALETTE.length];
  }
  // Shared across model-over-time lines and pie slices so the same identity
  // keeps the same color on every canvas and across poll reorders.
  const identityPaletteAssigned = {};
  function identityColor(kind, key) {
    if (!Charts || !Charts.paletteIndexForKey) return paletteColor(0);
    const slot =
      Charts.paletteSlotKey ? Charts.paletteSlotKey(kind, key) : String(key ?? "");
    return paletteColor(Charts.paletteIndexForKey(identityPaletteAssigned, slot));
  }

  // Breakdown rows become pie slices keyed by identity. Zero-token rows stay
  // in the legend with a "0" value so a provider/model with prompts or
  // sessions but no token spend does not vanish next to active summary cards.
  function pieRows(breakdown, kind) {
    // Keep zero-token rows: a provider/model with prompts or sessions but no
    // token spend would otherwise vanish from the pie, leaving an empty
    // "no usage" chart next to summary cards that clearly show activity.
    // Zero-token rows get zero-width slices and a "0" legend value.
    const slotKind = kind === "provider" ? "provider" : "model";
    return (breakdown || [])
      .map((row) => ({
        key: row.key,
        kind: slotKind,
        // Clamp negative totals (untrusted upstream accounting) to zero so
        // they cannot invert slice geometry or push model lines off-plot.
        value: Math.max(0, row.total_tokens || 0),
      }))
      .filter((row) => row.key != null && row.key !== "");
  }

  // Percentage labels must stay readable on the fixed palette in both
  // themes. Delegate to the chart-math WCAG relative-luminance helper so the
  // contrast decision is unit-tested; fall back to white when math is absent.
  function pieLabelColor(color) {
    return Charts && Charts.textColorOn ? Charts.textColorOn(color) : "#ffffff";
  }

  function fmtInt(n) {
    return Number(n || 0).toLocaleString();
  }

  function abbrev(n) {
    const v = Number(n || 0);
    if (v >= 1e9) return `${(v / 1e9).toFixed(1).replace(/\.0$/, "")}B`;
    if (v >= 1e6) return `${(v / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
    if (v >= 1e3) return `${(v / 1e3).toFixed(1).replace(/\.0$/, "")}k`;
    return String(v);
  }

  function integerTicks(max, target = 4) {
    return Charts.integerTicks(max, target);
  }

  // Draw in CSS pixels. The backing store is CSS size × devicePixelRatio, then
  // the context is scaled so 10px text is 10 CSS pixels (crisp on HiDPI).
  // Never treat canvas.width (device pixels) as a CSS fallback — that double-
  // scales after a hidden-tab layout pass.
  function fitCanvas(canvas, cssHeight) {
    const dpr = window.devicePixelRatio || 1;
    const metrics = Charts.fitCanvasMetrics(
      canvas.clientWidth,
      canvas.__cssW,
      800,
      dpr,
      cssHeight || 220,
    );
    canvas.__cssW = metrics.cssW;
    canvas.__cssH = metrics.cssH;
    if (canvas.width !== metrics.bufferW || canvas.height !== metrics.bufferH) {
      canvas.width = metrics.bufferW;
      canvas.height = metrics.bufferH;
    }
    const ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx, cssW: metrics.cssW, cssH: metrics.cssH, dpr };
  }

  let chartResizeRaf = null;
  function scheduleChartResize() {
    if (chartResizeRaf) return;
    chartResizeRaf = requestAnimationFrame(() => {
      chartResizeRaf = null;
      if (activeTab !== "analytics") return;
      renderAnalyticsPresentation();
    });
  }
  window.addEventListener("resize", scheduleChartResize);

  function tooltipBox(canvas) {
    const wrap = canvas.closest(".chart-wrap");
    return wrap ? wrap.querySelector(".chart-tooltip") : null;
  }

  function showChartTooltip(canvas, clientX, clientY, content) {
    const tip = tooltipBox(canvas);
    if (!tip) return;
    tip.replaceChildren(content);
    tip.hidden = false;
    const wrap = canvas.closest(".chart-wrap");
    const rect = wrap.getBoundingClientRect();
    const pad = 12;
    let left = clientX - rect.left + pad;
    let top = clientY - rect.top - tip.offsetHeight - 8;
    if (top < 2) top = clientY - rect.top + pad;
    if (left + tip.offsetWidth > rect.width - 2) {
      left = clientX - rect.left - tip.offsetWidth - pad;
    }
    tip.style.left = `${Math.max(2, left)}px`;
    tip.style.top = `${Math.max(2, top)}px`;
  }

  function hideChartTooltip(canvas) {
    const tip = tooltipBox(canvas);
    if (tip) tip.hidden = true;
  }

  function dismissChartHoverUi(canvas) {
    hideChartTooltip(canvas);
    announceChartData(canvas, Charts ? Charts.liveRegionText(-1, "") : "");
  }

  function tooltipRowsEl(rows, formatValue) {
    const format = typeof formatValue === "function" ? formatValue : fmtInt;
    return rows.map(([label, value, color]) => {
      const row = document.createElement("div");
      row.className = "tt-row";
      const key = document.createElement("span");
      key.className = "tt-key";
      if (color) {
        const swatch = document.createElement("span");
        swatch.className = "tt-swatch";
        swatch.style.background = color;
        key.append(swatch);
      }
      key.append(document.createTextNode(label));
      const val = document.createElement("span");
      val.className = "tt-val";
      val.textContent = format(value);
      row.append(key, val);
      return row;
    });
  }

  function tooltipRowsFor(point, colors, hasCached) {
    return [
      ["Total tokens", point.total_tokens || 0, colors.tokens],
      ["Input tokens", point.input_tokens || 0, colors.input],
      ...(hasCached ? [["Cached tokens", point.cached_tokens || 0, colors.cached]] : []),
      ["Output tokens", point.output_tokens || 0, colors.output],
      ["Prompts", point.prompts || 0, colors.prompts],
      ["Sessions", point.sessions || 0, colors.sessions],
    ];
  }

  function tooltipEl(titleText, rows, formatValue) {
    const frag = document.createDocumentFragment();
    const title = document.createElement("div");
    title.className = "tt-title";
    title.textContent = titleText;
    frag.append(title);
    frag.append(...tooltipRowsEl(rows, formatValue));
    return frag;
  }

  // Key-only tooltip row (no numeric value). Used for empty-bucket copy and
  // overflow counts so those lines stay on the same DOM contract as line/bar
  // tooltips instead of injecting HTML strings.
  function tooltipNoteRow(text) {
    const row = document.createElement("div");
    row.className = "tt-row";
    const key = document.createElement("span");
    key.className = "tt-key";
    key.textContent = text;
    row.append(key);
    return row;
  }

  function lineTooltipEl(point, labelStyle, colors, hasCached) {
    return tooltipEl(formatBucketLabel(point.ts, labelStyle), tooltipRowsFor(point, colors, hasCached));
  }

  function barTooltipEl(row, labelStyle, colors, hasCached) {
    return tooltipEl(row.key || formatBucketLabel(row.ts, labelStyle), tooltipRowsFor(row, colors, hasCached));
  }

  function tooltipSummary(point, labelStyle, hasCached) {
    const label = formatBucketLabel(point.ts, labelStyle);
    return `${label}: ` + tooltipRowsFor(point, {}, hasCached)
      .map(([name, value]) => `${name} ${fmtInt(value)}`)
      .join(", ");
  }

  function announceChartData(canvas, text) {
    const wrap = canvas && canvas.closest ? canvas.closest(".chart-wrap") : null;
    const live = wrap ? wrap.querySelector(".chart-live") : null;
    if (!live) return;
    const value = text == null ? "" : String(text);
    if (!Charts) {
      if (live.textContent !== value) live.textContent = value;
      return;
    }
    const next = Charts.announceIfChanged(live.textContent, value);
    if (next.changed) live.textContent = next.text;
  }

  function chartCanvases() {
    return [
      $("#chart-line"),
      $("#chart-bar"),
      $("#chart-model-sessions"),
      $("#chart-model-prompts"),
      $("#chart-model-cache-rate"),
      $("#chart-pie-provider"),
      $("#chart-pie-model"),
      $("#chart-pie-provider-models"),
    ].filter(Boolean);
  }

  function deactivateCharts({ except } = {}) {
    for (const canvas of chartCanvases()) {
      if (except && canvas === except) continue;
      const state = canvas.__chart;
      if (Charts) {
        const next = Charts.chartInputStep(
          {
            points: state ? chartPoints(state) : [],
            hoverTs: state ? state.hoverTs : null,
            inputMode: state && state.inputMode ? state.inputMode : "pointer",
            hasMouse: !!canvas.__mouse,
          },
          { type: "deactivate" },
        );
        canvas.__mouse = next.hasMouse ? canvas.__mouse : null;
        if (state) {
          state.inputMode = next.inputMode;
          state.hoverTs = next.hoverTs;
          if (state.kind === "pie") {
            // Positional hoverTs is only an index; the slice key is what
            // survives value-driven reorders across polls. Deactivation must
            // clear it here exactly like applyChartInput does, otherwise the
            // next pie redraw re-derives a live hover from the stale key.
            state.hoverKey =
              state.hoverTs == null
                ? null
                : state.rows[state.hoverTs]
                  ? state.rows[state.hoverTs].key
                  : null;
          }
        }
      } else {
        canvas.__mouse = null;
      }
      dismissChartHoverUi(canvas);
      if (document.activeElement === canvas) canvas.blur();
      else if (state) redrawChart(canvas, state);
    }
  }

  function chartToClient(canvas, cssX, cssY) {
    const rect = canvas.getBoundingClientRect();
    const cssW = canvas.__cssW || canvas.clientWidth || 1;
    const cssH = canvas.__cssH || canvas.clientHeight || 220;
    return {
      x: rect.left + (cssX * rect.width) / cssW,
      y: rect.top + (cssY * rect.height) / cssH,
    };
  }

  function showLineTooltipFor(canvas, state, idx) {
    const point = state.series[idx];
    const g = state.geometry;
    const anchorY = lineChartTooltipAnchorY(point, g, state.hasCachedData);
    const pos = chartToClient(canvas, g.xAt(point.ts), anchorY);
    showChartTooltip(canvas, pos.x, pos.y, lineTooltipEl(point, state.labelStyle, chartColors(), state.hasCachedData));
    announceChartData(canvas, Charts.liveRegionText(idx, tooltipSummary(point, state.labelStyle, state.hasCachedData)));
  }

  // Keyboard/non-pointer tooltips anchor to the highest token-axis marker for the
  // bucket (total vs cached) so the popup stays near the rings being described.
  function lineChartTooltipAnchorY(point, geometry, hasCachedData) {
    const anchorTokens = Charts.tokenAxisAnchorTokens(
      point.total_tokens,
      point.cached_tokens,
      hasCachedData,
    );
    return geometry.yTokens(anchorTokens);
  }

  function showBarTooltipFor(canvas, state, idx) {
    const row = state.rows[idx];
    const g = state.geometry;
    // Bars paint total tokens only; the axis does not include cached, so the
    // keyboard popup stays on the drawn bar even when cached exceeds total.
    const y = Charts.barAnchorY(row.total_tokens || 0, g.top, g.plotH, g.padT);
    const pos = chartToClient(canvas, g.xAt(idx) + (g.barW || 0) / 2, y);
    showChartTooltip(canvas, pos.x, pos.y, barTooltipEl(row, state.labelStyle, chartColors(), state.hasCachedData));
    announceChartData(canvas, Charts.liveRegionText(idx, tooltipSummary(row, state.labelStyle, state.hasCachedData)));
  }

  function tooltipFromPayload(payload, formatValue) {
    const plan =
      Charts && Charts.tooltipRenderPlan
        ? Charts.tooltipRenderPlan(payload)
        : null;
    if (!plan || plan.kind === "empty") return document.createDocumentFragment();
    const rows = (plan.rows || []).map((row) => [
      row.key,
      row.value,
      row.color && row.color.type === "key"
        ? identityColor(row.color.kind, row.color.key)
        : null,
    ]);
    const frag = tooltipEl(plan.title, rows, formatValue);
    if (plan.note) frag.append(tooltipNoteRow(plan.note));
    return frag;
  }

  function modelMetricFormat(metric) {
    if (metric === "cache_rate" && Charts && Charts.formatCacheRate) {
      return Charts.formatCacheRate;
    }
    return fmtInt;
  }

  function modelTooltipView(models, idx, labelStyle, metric) {
    if (!models.length || !models[0].points[idx]) {
      return { content: document.createDocumentFragment(), summary: "" };
    }
    const title = formatBucketLabel(models[0].points[idx].ts, labelStyle);
    const payload = Charts.modelTooltipPayload(models, idx, title, metric);
    const format = modelMetricFormat(metric);
    return {
      content: tooltipFromPayload(payload, format),
      summary: Charts.modelTooltipSummary
        ? Charts.modelTooltipSummary(payload, metric, format, 4)
        : "",
    };
  }

  function showModelTooltipFor(canvas, state, idx) {
    const g = state.geometry;
    const pos = chartToClient(canvas, g.xAt(g.buckets[idx].ts), g.yAt(0));
    const view = modelTooltipView(state.series, idx, state.labelStyle, state.metric);
    showChartTooltip(canvas, pos.x, pos.y, view.content);
    announceChartData(canvas, Charts.liveRegionText(idx, view.summary));
  }

  function pieTooltipView(row, total) {
    const payload = Charts.pieTooltipPayload(row, total);
    return {
      content: tooltipFromPayload(payload),
      summary: Charts.pieTooltipSummary
        ? Charts.pieTooltipSummary(payload, fmtInt)
        : "",
    };
  }

  function showPieTooltipFor(canvas, state, idx) {
    const g = state.geometry;
    const mid = Charts.pieMidAngle(g.slices[idx]);
    const pos = chartToClient(
      canvas,
      g.cx + Math.cos(mid) * g.r * 0.6,
      g.cy + Math.sin(mid) * g.r * 0.6,
    );
    const view = pieTooltipView(state.rows[idx], state.total);
    showChartTooltip(canvas, pos.x, pos.y, view.content);
    announceChartData(canvas, Charts.liveRegionText(idx, view.summary));
  }

  // Hover state is stored as the bucket's timestamp (`hoverTs`) and resolved
  // against the current data on every redraw. Positional indices go stale the
  // moment the series changes (poll, range switch, theme redraw), which caused
  // out-of-bounds reads and tooltips on the wrong point. Identity resolution
  // makes hover follow the actual data point and drop cleanly when the point
  // disappears.
  function resolveLineIdx(state) {
    if (!state.geometry) return -1;
    return Charts.resolveIdxByTs(state.series, state.hoverTs);
  }

  function resolveBarIdx(state) {
    if (!state.geometry) return -1;
    return Charts.resolveIdxByTs(state.rows, state.hoverTs);
  }

  function resolveModelIdx(state) {
    if (!state.geometry || !state.geometry.buckets) return -1;
    return Charts.resolveIdxByTs(state.geometry.buckets, state.hoverTs);
  }

  function resolvePieIdx(state) {
    if (!state.geometry || !state.rows.length) return -1;
    const idx = state.hoverTs;
    // JS coercion makes `null >= 0` true, which turned a no-hover pie state
    // into `null` (not -1) and crashed the hover-ring block on slices[null].
    // Guard explicitly so no-hover always means -1.
    return typeof idx === "number" && idx >= 0 && idx < state.rows.length ? idx : -1;
  }

  // Uniform point list across chart kinds so the shared focus/keyboard
  // handlers never assume a property that only one chart stores.
  function chartPoints(state) {
    if (state.kind === "line") return state.series;
    if (state.kind === "bar") return state.rows;
    // Pie identity resolves through positional hoverTs over pseudo points;
    // the slice key keeps the selection stable across value-driven reorders.
    if (state.kind === "pie") return state.rows.map((row, i) => ({ ts: i }));
    if (state.kind === "model") {
      return state.geometry && state.geometry.buckets ? state.geometry.buckets : [];
    }
    return state.series;
  }

  function redrawChart(canvas, state) {
    if (!state) return;
    if (state.kind === "line") drawLineChart(canvas, state.series, state.range);
    else if (state.kind === "bar") drawBarChart(canvas, state.rows, state.range);
    else if (state.kind === "model") {
      drawModelUsageChart(canvas, state.series, state.metric, state.range);
    } else if (state.kind === "pie") {
      // Interaction redraws must feed the full row set (including zero-token
      // rows kept in the legend); state.rows is the active, slice-able subset
      // used for hover/keyboard resolution.
      drawPieChart(canvas, state.legendRows || state.rows, state.pieOptions || {});
    }
  }

  function attachChartHover(canvas) {
    if (canvas.dataset.chartHover) return;
    canvas.dataset.chartHover = "1";

    function chartInputState() {
      const state = canvas.__chart;
      return {
        points: state ? chartPoints(state) : [],
        hoverTs: state ? state.hoverTs : null,
        inputMode: state && state.inputMode ? state.inputMode : "pointer",
        hasMouse: !!canvas.__mouse,
      };
    }

    function applyChartInput(next) {
      const state = canvas.__chart;
      canvas.__mouse = next.hasMouse ? canvas.__mouse : null;
      if (!state) return next;
      state.inputMode = next.inputMode;
      if (Object.prototype.hasOwnProperty.call(next, "hoverTs")) {
        state.hoverTs = next.hoverTs;
      }
      if (state.kind === "pie") {
        // Positional hoverTs is only an index into the current rows; the
        // slice key is what survives value-driven reorders across polls.
        state.hoverKey =
          state.hoverTs == null ? null : state.rows[state.hoverTs] ? state.rows[state.hoverTs].key : null;
      }
      return next;
    }

    canvas.addEventListener("mousemove", (event) => {
      const state = canvas.__chart;
      const prevHover = state ? state.hoverTs : null;
      const prevMode = state && state.inputMode ? state.inputMode : "pointer";
      const next = Charts.chartInputStep(chartInputState(), {
        type: "mousemove",
        hitTs: pointerHitTs(canvas, event, state),
      });
      applyChartInput(next);
      canvas.__mouse = next.hasMouse ? { x: event.clientX, y: event.clientY } : null;
      if (next.claimExclusive) deactivateCharts({ except: canvas });
      if (!state) return;
      if (prevHover !== state.hoverTs || prevMode !== state.inputMode) {
        if (state.hoverTs == null) dismissChartHoverUi(canvas);
        redrawChart(canvas, state);
        return;
      }
      if (state.kind === "line") handleLineChartHover(canvas, event, state);
      else if (state.kind === "bar") handleBarChartHover(canvas, event, state);
      else if (state.kind === "model") handleModelChartHover(canvas, event, state);
      else if (state.kind === "pie") handlePieChartHover(canvas, event, state);
    });
    canvas.addEventListener("mouseleave", () => {
      const next = Charts.chartInputStep(chartInputState(), { type: "mouseleave" });
      applyChartInput(next);
      const state = canvas.__chart;
      if (!state || next.inputMode === "keyboard") return;
      dismissChartHoverUi(canvas);
      redrawChart(canvas, state);
    });
    canvas.addEventListener("focus", () => {
      const next = Charts.chartInputStep(chartInputState(), { type: "focus" });
      applyChartInput(next);
      if (next.claimExclusive) deactivateCharts({ except: canvas });
      const state = canvas.__chart;
      if (!state || next.hoverTs == null) return;
      redrawChart(canvas, state);
    });
    canvas.addEventListener("blur", () => {
      const next = Charts.chartInputStep(chartInputState(), { type: "blur" });
      applyChartInput(next);
      dismissChartHoverUi(canvas);
      const state = canvas.__chart;
      if (state) redrawChart(canvas, state);
    });
    canvas.addEventListener("keydown", (event) => {
      const next = Charts.chartInputStep(chartInputState(), { type: "keydown", key: event.key });
      if (next.preventDefault) event.preventDefault();
      applyChartInput(next);
      const state = canvas.__chart;
      if (!state || !next.preventDefault) return;
      redrawChart(canvas, state);
    });
  }

  function drawLineChart(canvas, series, range) {
    if (!Charts.shouldPaintCharts(canvas.clientWidth, canvas.__cssW)) return;
    const { ctx, cssW: w, cssH: h } = fitCanvas(canvas, 220);
    ctx.clearRect(0, 0, w, h);
    if (!series.length) {
      canvas.__chart = { kind: "line", series: [], range, geometry: null, hoverTs: null, inputMode: "pointer", labelStyle: "time", hasCachedData: false };
      dismissChartHoverUi(canvas);
      return;
    }
    const colors = chartColors();
    const tokenVals = series.map((p) => p.total_tokens || 0);
    const cachedVals = series.map((p) => p.cached_tokens || 0);
    const promptVals = series.map((p) => p.prompts || 0);
    const sessionVals = series.map((p) => p.sessions || 0);
    // Each series scales independently so a small-magnitude series (e.g.
    // sessions against prompts) is not flattened into the baseline.
    // Cached tokens share the token axis, so the scale covers both totals and
    // cached values. Upstreams can report cache reads outside input tokens
    // (e.g. prompt_cache_hit_tokens), so cached may exceed the total max;
    // scaling to the union keeps the chart and tooltip on the same numbers.
    const tokens = integerTicks(Math.max(1, ...tokenVals, ...cachedVals));
    const hasCachedData = cachedVals.some((value) => value > 0);
    const prompts = integerTicks(Math.max(1, ...promptVals));
    const sessions = integerTicks(Math.max(1, ...sessionVals));
    const tsMin = series[0].ts;
    const tsMax = series[series.length - 1].ts;
    const tsSpan = Math.max(1, tsMax - tsMin);

    // Legend chips live in the top padding band above the plot so they never
    // cover the drawn series or the right-hand axis labels. Pack them against
    // the actual (possibly shrunk) plot padding and cap at two rows so wrapping
    // cannot collapse the plot or drop a drawn series.
    const legendItems = [
      ["Total tokens", colors.tokens],
      ...(hasCachedData ? [["Cached tokens", colors.cached]] : []),
      ["Prompts", colors.prompts],
      ["Sessions", colors.sessions],
    ];
    ctx.font = "10px system-ui";
    ctx.textBaseline = "middle";
    const legendGap = 6;
    const wantL = 46;
    const wantR = 88;
    const baseLayout = Charts.layoutChartPlot(w, h, {
      padL: wantL,
      padR: wantR,
      padT: 30,
      padB: 26,
    });
    // Axis titles sit in this same top band, centered on each axis x. Start
    // the legend after the measured "tokens" title so the two cannot overlap.
    const titleHalf = ctx.measureText("tokens").width / 2;
    const legendStartX = baseLayout.padL + titleHalf + legendGap;
    const legendBudget = w - baseLayout.padR - legendStartX;
    const legendLayout = Charts.layoutLegendChips(
      legendItems,
      (text) => ctx.measureText(text).width,
      legendBudget,
      { gap: legendGap, maxRows: 2 },
    );
    const legendRows = legendLayout.rows;
    const legendPadExtra = Charts.legendSecondRowPad(legendLayout);

    const { padT, padL, padR, plotW, plotH } = Charts.layoutChartPlot(w, h, {
      padL: wantL,
      padR: wantR,
      padT: 30 + legendPadExtra,
      padB: 26,
    });
    const axisX = {
      tokens: padL,
      prompts: w - padR + 44,
      sessions: w - 12,
    };
    const xAt = (ts) => padL + ((ts - tsMin) / tsSpan) * plotW;
    const yFor = (top) => (v) => padT + (1 - v / top) * plotH;
    const yTokens = yFor(tokens.top);
    const yPrompts = yFor(prompts.top);
    const ySessions = yFor(sessions.top);
    const prev = canvas.__chart;
    canvas.__chart = {
      kind: "line",
      series,
      range,
      geometry: { xAt, yTokens, yPrompts, ySessions, padT, plotH, padL, padR, axisX, tokens, prompts, sessions, cssW: w },
      hoverTs: Charts.reconcileHoverTs(series, prev && prev.kind === "line" ? prev.hoverTs : null),
      inputMode: prev && prev.kind === "line" ? prev.inputMode || "pointer" : "pointer",
      labelStyle: Charts.bucketLabelStyle(series),
      hasCachedData,
    };

    // Gridlines and left (token) axis labels at integer ticks.
    ctx.font = "10px system-ui";
    ctx.textBaseline = "middle";
    for (const tick of tokens.ticks) {
      const y = yTokens(tick);
      ctx.strokeStyle = colors.grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(w - padR, y);
      ctx.stroke();
      ctx.fillStyle = colors.muted;
      ctx.textAlign = "right";
      ctx.fillText(abbrev(tick), padL - 6, y);
    }

    // Right axes: prompts and sessions each get their own scale and label.
    ctx.textAlign = "center";
    ctx.fillStyle = colors.tokens;
    ctx.fillText("tokens", padL, 14);
    for (const [key, ticks, color] of [["prompts", prompts, colors.prompts], ["sessions", sessions, colors.sessions]]) {
      const ax = axisX[key];
      ctx.fillStyle = colors.muted;
      ctx.textAlign = "right";
      for (const tick of ticks.ticks) {
        ctx.fillText(abbrev(tick), ax - 6, yFor(ticks.top)(tick));
      }
      ctx.fillStyle = color;
      ctx.textAlign = "center";
      ctx.fillText(key, ax, 14);
    }

    function strokeSeries(vals, yFn, color, dashed, skipZero) {
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      if (dashed) ctx.setLineDash([4, 3]);
      ctx.beginPath();
      let started = false;
      vals.forEach((val, i) => {
        // Zero-value points are gaps: a dashed baseline run would read as
        // cached activity where there is none (the common sparse-cache case).
        if (skipZero && !(val > 0)) {
          started = false;
          return;
        }
        const x = xAt(series[i].ts);
        const y = yFn(val);
        if (!started) {
          ctx.moveTo(x, y);
          started = true;
        } else {
          ctx.lineTo(x, y);
        }
      });
      ctx.stroke();
      if (dashed) ctx.setLineDash([]);
    }

    function drawDots(vals, yFn, color, radius, skipZero) {
      ctx.fillStyle = color;
      vals.forEach((val, i) => {
        // Zero-value markers along the baseline read as false activity (the
        // common case for cached tokens on buckets without cache hits).
        if (skipZero && !(val > 0)) return;
        ctx.beginPath();
        ctx.arc(xAt(series[i].ts), yFn(val), radius || 3, 0, Math.PI * 2);
        ctx.fill();
      });
    }

    strokeSeries(tokenVals, yTokens, colors.tokens);
    if (hasCachedData) {
      // Cached is drawn over the total line so it stays visible when the two
      // coincide on fully-cached buckets; the dashed pattern keeps the total
      // line identifiable through the gaps. Zero buckets are gaps in the
      // stroke so no baseline run implies cached activity where there is none.
      strokeSeries(cachedVals, yTokens, colors.cached, true, true);
    }
    strokeSeries(promptVals, yPrompts, colors.prompts);
    strokeSeries(sessionVals, ySessions, colors.sessions);
    drawDots(tokenVals, yTokens, colors.tokens);
    if (hasCachedData) {
      // The smaller cached dot sits inside the total dot, leaving a rim of the
      // total marker visible when they coincide on fully-cached buckets.
      drawDots(cachedVals, yTokens, colors.cached, 2, true);
    }
    drawDots(promptVals, yPrompts, colors.prompts);
    drawDots(sessionVals, ySessions, colors.sessions);

    // Legend chips in the reserved top band. Clip to that band so a packing
    // overflow (min swatches wider than the budget) cannot cover the plot or
    // the right-axis column.
    const legendClip = Charts.legendPaintClip(legendStartX, legendBudget, padT);
    ctx.save();
    ctx.beginPath();
    ctx.rect(legendClip.x, legendClip.y, legendClip.width, legendClip.height);
    ctx.clip();
    legendRows.forEach((chips, rowIndex) => {
      let lx = legendStartX;
      const ly = Charts.legendChipRowY(rowIndex);
      for (const chip of chips) {
        const pad = chip.pad != null ? chip.pad : 4;
        const swatch = chip.swatch != null ? chip.swatch : 8;
        const labelX = chip.labelX != null ? chip.labelX : pad + swatch + 4;
        ctx.fillStyle = colors.surface;
        ctx.strokeStyle = colors.grid;
        ctx.fillRect(lx, ly, chip.width, 16);
        ctx.strokeRect(lx, ly, chip.width, 16);
        if (chip.width >= pad + swatch) {
          ctx.fillStyle = chip.color;
          ctx.fillRect(lx + pad, ly + pad, swatch, swatch);
        }
        ctx.fillStyle = colors.text;
        ctx.textAlign = "left";
        if (chip.label && chip.width >= labelX) {
          ctx.fillText(chip.label, lx + labelX, ly + 8);
        }
        lx += chip.width + legendGap;
      }
    });
    ctx.restore();

    // Hover overlay and tooltip, resolved by bucket identity so redraws with
    // changed data can never leave a stale index behind.
    const state = canvas.__chart;
    const idx = resolveLineIdx(state);
    if (idx >= 0) {
      renderLineHover(canvas, state, idx);
      if (Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) {
        showChartTooltip(canvas, canvas.__mouse.x, canvas.__mouse.y, lineTooltipEl(series[idx], state.labelStyle, colors, hasCachedData));
        announceChartData(canvas, Charts.liveRegionText(idx, tooltipSummary(series[idx], state.labelStyle, hasCachedData)));
      } else {
        showLineTooltipFor(canvas, state, idx);
      }
    } else {
      dismissChartHoverUi(canvas);
    }
  }

  function pointerHitTs(canvas, event, state) {
    if (!state || !state.geometry) return null;
    const rect = canvas.getBoundingClientRect();
    const mx = Charts.pointerCssX(
      event.clientX,
      rect.left,
      rect.width,
      state.geometry.cssW || canvas.__cssW,
    );
    if (state.kind === "line") {
      const best = Charts.nearestIdxByX(
        state.series.map((point) => state.geometry.xAt(point.ts)),
        mx,
        14,
      );
      return best < 0 ? null : state.series[best].ts;
    }
    if (state.kind === "model") {
      const buckets = state.geometry.buckets || [];
      const best = Charts.nearestIdxByX(
        buckets.map((bucket) => state.geometry.xAt(bucket.ts)),
        mx,
        14,
      );
      return best < 0 ? null : buckets[best].ts;
    }
    if (state.kind === "pie") {
      const g = state.geometry;
      const cssH = g.cssH || canvas.__cssH || 260;
      const my = Charts.pointerCssY(event.clientY, rect.top, rect.height, cssH);
      // The hover ring is painted at radius + 4, so the hit zone must extend
      // to the same band or moving the pointer onto the ring dismisses the
      // selection it indicates.
      const idx = Charts.pieSliceIndexAt(g.cx, g.cy, g.r + 4, 0, g.slices, mx, my);
      return idx < 0 ? null : idx;
    }
    const idx = Charts.barIndexAtX(mx - state.geometry.padL, state.geometry.slot, state.rows.length);
    return idx < 0 ? null : state.rows[idx].ts;
  }

  function handleLineChartHover(canvas, event, state) {
    if (!Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) return;
    const idx = resolveLineIdx(state);
    if (idx < 0) return;
    const point = state.series[idx];
    showChartTooltip(canvas, event.clientX, event.clientY, lineTooltipEl(point, state.labelStyle, chartColors(), state.hasCachedData));
    announceChartData(canvas, Charts.liveRegionText(idx, tooltipSummary(point, state.labelStyle, state.hasCachedData)));
  }

  function renderLineHover(canvas, state, idx) {
    if (idx < 0 || idx >= state.series.length || !state.geometry) return;
    const point = state.series[idx];
    const colors = chartColors();
    const ctx = canvas.getContext("2d");
    const { xAt, yTokens, yPrompts, ySessions, padT, plotH } = state.geometry;
    const x = xAt(point.ts);
    ctx.strokeStyle = colors.muted;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(x, padT);
    ctx.lineTo(x, padT + plotH);
    ctx.stroke();
    ctx.setLineDash([]);
    const ring = (y, color, radius) => {
      ctx.beginPath();
      ctx.arc(x, y, radius || 5, 0, Math.PI * 2);
      ctx.fillStyle = colors.surface;
      ctx.fill();
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.stroke();
    };
    ring(yTokens(point.total_tokens || 0), colors.tokens);
    ring(yPrompts(point.prompts || 0), colors.prompts);
    ring(ySessions(point.sessions || 0), colors.sessions);
    if ((point.cached_tokens || 0) > 0) {
      // The cached ring is drawn last so no later surface-filled ring can
      // overpaint it: at coincidence the r=3 cached ring stays visible inside
      // the r=5 total ring. Skip zero-cached buckets so no baseline ring is
      // implied where no cached activity exists.
      ring(yTokens(point.cached_tokens || 0), colors.cached, 3);
    }
  }

  function drawBarChart(canvas, rows, range) {
    if (!Charts.shouldPaintCharts(canvas.clientWidth, canvas.__cssW)) return;
    const { ctx, cssW: w, cssH: h } = fitCanvas(canvas, 220);
    ctx.clearRect(0, 0, w, h);
    if (!rows.length) {
      canvas.__chart = { kind: "bar", rows: [], range, geometry: null, hoverTs: null, inputMode: "pointer", labelStyle: "time", hasCachedData: false };
      dismissChartHoverUi(canvas);
      return;
    }
    const { padT, padB, padL, padR, plotW, plotH } = Charts.layoutChartPlot(w, h, {
      padL: 46,
      padR: 16,
      padT: 30,
      padB: 26,
    });
    const colors = chartColors();
    // The bar chart paints only total-token bars, so the axis scales to the
    // drawn series; the hover tooltip still reports the bucket's cached tokens
    // as a text readout even when that value exceeds the axis top.
    const max = Math.max(1, ...rows.map((r) => r.total_tokens || 0));
    const hasCachedData = rows.some((r) => (r.cached_tokens || 0) > 0);
    const ticks = integerTicks(max);
    const { barW, barGap, slot } = Charts.barSlotLayout(plotW, rows.length);
    const xAt = (i) => padL + i * slot;
    const yAt = (v) => padT + (1 - v / ticks.top) * plotH;
    const prev = canvas.__chart;
    canvas.__chart = {
      kind: "bar",
      rows,
      range,
      geometry: { xAt, yAt, barW, barGap, slot, plotH, padT, padL, max, top: ticks.top, cssW: w },
      hoverTs: Charts.reconcileHoverTs(rows, prev && prev.kind === "bar" ? prev.hoverTs : null),
      inputMode: prev && prev.kind === "bar" ? prev.inputMode || "pointer" : "pointer",
      labelStyle: Charts.bucketLabelStyle(rows),
      hasCachedData,
    };

    // Gridlines with integer token-count labels on the left axis.
    ctx.font = "10px system-ui";
    ctx.textBaseline = "middle";
    for (const tick of ticks.ticks) {
      const y = yAt(tick);
      ctx.strokeStyle = colors.grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(w - padR, y);
      ctx.stroke();
      ctx.fillStyle = colors.muted;
      ctx.textAlign = "right";
      ctx.fillText(abbrev(tick), padL - 6, y);
    }

    rows.forEach((r, i) => {
      const val = r.total_tokens || 0;
      const painted = Charts.barPaintRect(val, ticks.top, plotH);
      const x = xAt(i);
      const y = padT + painted.y;
      ctx.fillStyle = colors.bar;
      if (barW > 0 && painted.barH >= 1) {
        ctx.fillRect(x, y, barW, painted.barH);
      }
      // Value label only when it fits inside the bar slot; on dense ranges the
      // tooltip (mouse or keyboard) carries the exact value instead of
      // overlapping neighbor labels.
      if (val > 0 && barW >= 1 && painted.barH >= 1) {
        ctx.font = "10px system-ui";
        const label = abbrev(val);
        const labelW = ctx.measureText(label).width;
        if (labelW <= barW + 4) {
          ctx.textAlign = "center";
          ctx.fillStyle = colors.muted;
          if (y - 4 >= padT + 8) {
            ctx.textBaseline = "bottom";
            ctx.fillText(label, x + barW / 2, y - 4);
          } else if (painted.barH >= 14) {
            ctx.textBaseline = "top";
            ctx.fillText(label, x + barW / 2, y + 2);
          }
        }
      }
      // Bucket labels; thin out when the canvas gets crowded.
      if (rows.length <= 40 || i % Math.ceil(rows.length / 40) === 0 || i === rows.length - 1) {
        ctx.font = "10px system-ui";
        ctx.textAlign = "center";
        ctx.textBaseline = "top";
        ctx.fillStyle = colors.muted;
        ctx.fillText(String(r.key || "").slice(0, 12), x + barW / 2, h - padB + 4);
      }
    });

    // Highlight and tooltip for the hovered/focused bucket, resolved by
    // identity so data changes can never leave a stale index behind.
    const hidx = resolveBarIdx(canvas.__chart);
    if (hidx >= 0) {
      const r = rows[hidx];
      const val = r.total_tokens || 0;
      const painted = Charts.barPaintRect(val, ticks.top, plotH);
      const x = xAt(hidx);
      const y = padT + painted.y;
      if (barW > 0 && painted.barH >= 1) {
        ctx.fillStyle = cssThemeColor("--accent-hover", colors.bar);
        ctx.fillRect(x, y, barW, painted.barH);
        ctx.strokeStyle = colors.tokens;
        ctx.lineWidth = 2;
        ctx.strokeRect(x, y, barW, painted.barH);
      }
      const state = canvas.__chart;
      if (Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) {
        showChartTooltip(canvas, canvas.__mouse.x, canvas.__mouse.y, barTooltipEl(rows[hidx], state.labelStyle, colors, hasCachedData));
        announceChartData(canvas, Charts.liveRegionText(hidx, tooltipSummary(rows[hidx], state.labelStyle, hasCachedData)));
      } else {
        showBarTooltipFor(canvas, state, hidx);
      }
    } else {
      dismissChartHoverUi(canvas);
    }
  }

  function handleBarChartHover(canvas, event, state) {
    if (!Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) return;
    const idx = resolveBarIdx(state);
    if (idx < 0) return;
    const row = state.rows[idx];
    showChartTooltip(canvas, event.clientX, event.clientY, barTooltipEl(row, state.labelStyle, chartColors(), state.hasCachedData));
    announceChartData(canvas, Charts.liveRegionText(idx, tooltipSummary(row, state.labelStyle, state.hasCachedData)));
  }

  function handleModelChartHover(canvas, event, state) {
    if (!Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) return;
    const idx = resolveModelIdx(state);
    if (idx < 0) return;
    const view = modelTooltipView(state.series, idx, state.labelStyle, state.metric);
    showChartTooltip(canvas, event.clientX, event.clientY, view.content);
    announceChartData(canvas, Charts.liveRegionText(idx, view.summary));
  }

  function handlePieChartHover(canvas, event, state) {
    if (!Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) return;
    const idx = resolvePieIdx(state);
    if (idx < 0) return;
    const view = pieTooltipView(state.rows[idx], state.total);
    showChartTooltip(canvas, event.clientX, event.clientY, view.content);
    announceChartData(canvas, Charts.liveRegionText(idx, view.summary));
  }

  function legendElFor(canvas) {
    const box = canvas.closest(".chart-box");
    return box ? box.querySelector(".chart-legend") : null;
  }

  function renderChartLegend(container, items) {
    if (!container) return;
    container.innerHTML = "";
    for (const item of items) {
      const chip = document.createElement("span");
      chip.className = "legend-chip";
      const swatch = document.createElement("span");
      swatch.className = "legend-swatch";
      swatch.style.background = item.color;
      const label = document.createElement("span");
      label.className = "legend-label";
      label.textContent = item.key;
      const value = document.createElement("span");
      value.className = "legend-value";
      const format = typeof item.format === "function" ? item.format : fmtInt;
      value.textContent = format(item.value);
      chip.append(swatch, label, value);
      container.append(chip);
    }
  }

  function drawChartEmpty(ctx, w, h, message) {
    ctx.font = "12px system-ui";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = cssThemeColor("--muted", "#71717a");
    ctx.fillText(message || "No data in this range.", w / 2, h / 2);
  }

  function modelTotal(model, metric) {
    // Cache rate is token-weighted over the window: cached / input, not a
    // mean of per-bucket rates. Sessions still need the backend total because
    // bucket-scoped distinct counts double-count sessions that span buckets.
    if (metric === "cache_rate") {
      const totals = model.totals || {};
      return Charts.cacheRatePercent(totals.cached_tokens, totals.input_tokens);
    }
    if (model.totals && model.totals[metric] != null) {
      const n = Number(model.totals[metric]);
      return Number.isFinite(n) ? n : 0;
    }
    return model.points.reduce(
      (sum, point) => sum + Charts.modelMetricValue(point, metric),
      0,
    );
  }

  function modelSeriesVisible(model, metric) {
    if (metric === "cache_rate") {
      const input = Number(model.totals && model.totals.input_tokens);
      return Number.isFinite(input) && input > 0;
    }
    return modelTotal(model, metric) > 0;
  }

  // One line per model across the shared bucket window. Lines share the
  // metric's own scale so relative model usage is visible on the same axis.
  function drawModelUsageChart(canvas, modelSeries, metric, range) {
    if (!Charts.shouldPaintCharts(canvas.clientWidth, canvas.__cssW)) return;
    const { ctx, cssW: w, cssH: h } = fitCanvas(canvas, 220);
    ctx.clearRect(0, 0, w, h);
    const legend = legendElFor(canvas);
    const prev = canvas.__chart;
    const models = (modelSeries || [])
      .map((series) => ({
        model: series.model,
        points: series.points || [],
        totals: series.totals || {
          prompts: series.prompts || 0,
          sessions: series.sessions || 0,
          input_tokens: series.input_tokens || 0,
          output_tokens: series.output_tokens || 0,
          total_tokens: series.total_tokens || 0,
          cached_tokens: series.cached_tokens || 0,
        },
      }))
      .filter((series) => series.points.length && modelSeriesVisible(series, metric))
      .sort((a, b) => modelTotal(b, metric) - modelTotal(a, metric));
    if (!models.length) {
      canvas.__chart = {
        kind: "model",
        series: [],
        metric,
        range,
        geometry: null,
        hoverTs: null,
        inputMode: prev && prev.kind === "model" ? prev.inputMode || "pointer" : "pointer",
        labelStyle: "time",
      };
      if (legend) legend.innerHTML = "";
      drawChartEmpty(
        ctx,
        w,
        h,
        metric === "cache_rate"
          ? "No model cache rate in this range."
          : "No model usage in this range.",
      );
      dismissChartHoverUi(canvas);
      return;
    }
    const buckets = models[0].points.map((point) => ({ ts: point.ts }));
    const labelStyle = Charts.bucketLabelStyle(buckets);
    let maxVal = 1;
    for (const model of models) {
      for (const point of model.points) {
        const value = Charts.modelMetricValue(point, metric);
        if (value > maxVal) maxVal = value;
      }
    }
    const ticks = integerTicks(maxVal);
    const { padT, padB, padL, padR, plotW, plotH } = Charts.layoutChartPlot(w, h, {
      padL: 46,
      padR: 16,
      padT: 30,
      padB: 26,
    });
    const tsMin = buckets[0].ts;
    const tsMax = buckets[buckets.length - 1].ts;
    const tsSpan = Math.max(1, tsMax - tsMin);
    const xAt = (ts) => padL + ((ts - tsMin) / tsSpan) * plotW;
    const yAt = (value) => padT + (1 - value / ticks.top) * plotH;
    const colors = chartColors();
    canvas.__chart = {
      kind: "model",
      series: models,
      metric,
      range,
      labelStyle,
      geometry: {
        xAt,
        yAt,
        ticks,
        padT,
        padB,
        padL,
        padR,
        plotW,
        plotH,
        cssW: w,
        buckets,
      },
      hoverTs: Charts.reconcileHoverTs(
        buckets,
        prev && prev.kind === "model" ? prev.hoverTs : null,
      ),
      inputMode: prev && prev.kind === "model" ? prev.inputMode || "pointer" : "pointer",
    };

    ctx.font = "10px system-ui";
    ctx.textBaseline = "middle";
    for (const tick of ticks.ticks) {
      const y = yAt(tick);
      ctx.strokeStyle = colors.grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(w - padR, y);
      ctx.stroke();
      ctx.fillStyle = colors.muted;
      ctx.textAlign = "right";
      ctx.fillText(
        metric === "cache_rate" ? `${abbrev(tick)}%` : abbrev(tick),
        padL - 6,
        y,
      );
    }
    ctx.fillStyle = colors.muted;
    ctx.textAlign = "center";
    ctx.fillText(
      metric === "cache_rate"
        ? (Charts.modelMetricLabel ? Charts.modelMetricLabel(metric) : "cache rate")
        : metric,
      padL,
      10,
    );

    const n = buckets.length;
    // Thin the time labels to ~10 across any range width; label the first and
    // last bucket always so the window edges stay identifiable.
    const labelStep = Math.max(1, Math.ceil(n / 10));
    ctx.textBaseline = "top";
    for (let i = 0; i < n; i += 1) {
      if (i !== 0 && i !== n - 1 && i % labelStep !== 0) continue;
      ctx.fillStyle = colors.muted;
      ctx.textAlign = "center";
      ctx.fillText(
        String(formatBucketLabel(buckets[i].ts, labelStyle)).slice(0, 12),
        xAt(buckets[i].ts),
        h - padB + 4,
      );
    }

    models.forEach((model) => {
      const color = identityColor("model", model.model);
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.beginPath();
      let drawing = false;
      model.points.forEach((point) => {
        const x = xAt(point.ts);
        const y = yAt(Charts.modelMetricValue(point, metric));
        // Cache rate is undefined when a bucket has no input. Connecting
        // those zeros would paint a fake 0% dip (tooltips already say
        // "no cache rate"). Count metrics still connect through zeros.
        if (metric === "cache_rate" && !Charts.modelPointActive(point, metric)) {
          drawing = false;
          return;
        }
        if (!drawing) {
          ctx.moveTo(x, y);
          drawing = true;
        } else {
          ctx.lineTo(x, y);
        }
      });
      ctx.stroke();
    });
    if (n === 1 || models.length <= 6) {
      models.forEach((model) => {
        ctx.fillStyle = identityColor("model", model.model);
        model.points.forEach((point) => {
          if (!Charts.modelPointActive(point, metric)) return;
          ctx.beginPath();
          ctx.arc(xAt(point.ts), yAt(Charts.modelMetricValue(point, metric)), 3, 0, Math.PI * 2);
          ctx.fill();
        });
      });
    }

    renderChartLegend(
      legend,
      models.map((model) => ({
        key: model.model,
        color: identityColor("model", model.model),
        value: modelTotal(model, metric),
        format: modelMetricFormat(metric),
      })),
    );

    const state = canvas.__chart;
    const idx = resolveModelIdx(state);
    if (idx >= 0) {
      renderModelHover(canvas, state, idx);
      if (Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) {
        const view = modelTooltipView(models, idx, labelStyle, metric);
        showChartTooltip(canvas, canvas.__mouse.x, canvas.__mouse.y, view.content);
        announceChartData(canvas, Charts.liveRegionText(idx, view.summary));
      } else {
        showModelTooltipFor(canvas, state, idx);
      }
    } else {
      dismissChartHoverUi(canvas);
    }
  }

  function renderModelHover(canvas, state, idx) {
    if (idx < 0 || !state.geometry || !state.geometry.buckets[idx]) return;
    const colors = chartColors();
    const ctx = canvas.getContext("2d");
    const { xAt, yAt, padT, plotH, buckets } = state.geometry;
    const x = xAt(buckets[idx].ts);
    ctx.strokeStyle = colors.muted;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(x, padT);
    ctx.lineTo(x, padT + plotH);
    ctx.stroke();
    ctx.setLineDash([]);
    state.series.forEach((model) => {
      const point = model.points[idx];
      if (!Charts.modelPointActive(point, state.metric)) return;
      const y = yAt(Charts.modelMetricValue(point, state.metric));
      ctx.beginPath();
      ctx.arc(x, y, 5, 0, Math.PI * 2);
      ctx.fillStyle = colors.surface;
      ctx.fill();
      ctx.strokeStyle = identityColor("model", model.model);
      ctx.lineWidth = 2;
      ctx.stroke();
    });
  }

  // Full pie (no donut hole) with a hover ring and percentage labels on
  // slices that have enough room. Slice colors follow identity, not sort
  // rank, so a poll that reorders wedges cannot swap colors.
  function drawPieChart(canvas, rows, options) {
    if (!Charts.shouldPaintCharts(canvas.clientWidth, canvas.__cssW)) return;
    const { ctx, cssW: w, cssH: h } = fitCanvas(canvas, 260);
    ctx.clearRect(0, 0, w, h);
    const legend = legendElFor(canvas);
    const opts = options || {};
    const prev = canvas.__chart;
    const sorted = (rows || []).slice().sort((a, b) => b.value - a.value);
    // Zero-token rows stay in the legend (a provider/model with prompts or
    // sessions but no token spend should not vanish), but they have no slice,
    // so the chart geometry, hover, and keyboard navigation only use rows with
    // value > 0.
    const legendRows = sorted.slice();
    const active = sorted.filter((row) => row.value > 0);
    if (!active.length) {
      canvas.__chart = {
        kind: "pie",
        rows: [],
        legendRows,
        total: 0,
        geometry: null,
        hoverTs: null,
        hoverKey: null,
        inputMode: prev && prev.kind === "pie" ? prev.inputMode || "pointer" : "pointer",
        pieOptions: opts,
        cssW: w,
      };
      // Zero-token rows still name the providers/models behind the activity;
      // keep them visible next to the empty-state message instead of wiping
      // the legend entirely.
      if (legend) {
        renderChartLegend(
          legend,
          legendRows.map((row) => ({
            key: row.key,
            color: identityColor(row.kind || "model", row.key),
            value: row.value,
          })),
        );
      }
      drawChartEmpty(ctx, w, h, opts.emptyText || "No data in this range.");
      dismissChartHoverUi(canvas);
      return;
    }
    const { slices, total } = Charts.pieSlices(active.map((row) => row.value));
    const pad = 30;
    const radius = Math.max(1, Math.min((w - pad * 2) / 2, (h - pad * 2) / 2));
    const cx = w / 2;
    const cy = h / 2;
    const colors = chartColors();
    const hoverKey = Charts.reconcilePieHover(
      active,
      prev && prev.kind === "pie" ? prev.hoverKey : null,
    );
    const hoverIdx = hoverKey == null ? -1 : active.findIndex((row) => row.key === hoverKey);
    // A poll redraw can reorder slices under a stationary pointer. When the
    // pointer still owns the hover, trust the slice under the cursor so the
    // tooltip never describes a slice the user is not pointing at. Padding
    // misses clear on purpose (same policy as mousemove via chartInputStep).
    let hitIdx = -1;
    const pointerOwned = Charts.tooltipFollowsPointer(
      prev && prev.kind === "pie" ? prev.inputMode || "pointer" : "pointer",
      canvas.__mouse,
    );
    if (pointerOwned && canvas.__mouse) {
      const rect = canvas.getBoundingClientRect();
      const mx = Charts.pointerCssX(
        canvas.__mouse.x,
        rect.left,
        rect.width,
        w,
      );
      const my = Charts.pointerCssY(canvas.__mouse.y, rect.top, rect.height, h);
      hitIdx = Charts.pieSliceIndexAt(cx, cy, radius + 4, 0, slices, mx, my);
    }
    const effectiveHoverIdx = Charts.effectivePieHoverIdx
      ? Charts.effectivePieHoverIdx(hoverIdx, pointerOwned, hitIdx)
      : pointerOwned
        ? hitIdx
        : hoverIdx;
    canvas.__chart = {
      kind: "pie",
      rows: active,
      legendRows,
      total,
      geometry: { cx, cy, r: radius, slices, cssW: w, cssH: h },
      hoverTs: effectiveHoverIdx >= 0 ? effectiveHoverIdx : null,
      hoverKey:
        effectiveHoverIdx >= 0 ? active[effectiveHoverIdx].key : null,
      inputMode: prev && prev.kind === "pie" ? prev.inputMode || "pointer" : "pointer",
      pieOptions: opts,
      cssW: w,
    };

    active.forEach((row, index) => {
      const slice = slices[index];
      ctx.beginPath();
      ctx.moveTo(cx, cy);
      ctx.arc(cx, cy, radius, slice.start, slice.end);
      ctx.closePath();
      ctx.fillStyle = identityColor(row.kind || "model", row.key);
      ctx.fill();
      ctx.strokeStyle = colors.surface;
      ctx.lineWidth = 2;
      ctx.stroke();
    });

    const state = canvas.__chart;
    const hidx = resolvePieIdx(state);
    if (hidx >= 0) {
      const slice = slices[hidx];
      ctx.beginPath();
      // Start on the outer arc (empty-path `arc` moveTos its first point).
      // A `moveTo(cx, cy)` would add radials through the center, so fill and
      // stroke would paint the whole wedge instead of the radius..radius+4 band.
      ctx.arc(cx, cy, radius + 4, slice.start, slice.end);
      ctx.arc(cx, cy, radius, slice.end, slice.start, true);
      ctx.closePath();
      ctx.fillStyle = identityColor(
        state.rows[hidx].kind || "model",
        state.rows[hidx].key,
      );
      ctx.fill();
      ctx.strokeStyle = colors.tokens;
      ctx.lineWidth = 1.5;
      ctx.stroke();
    }

    ctx.font = "10px system-ui";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    active.forEach((row, index) => {
      const slice = slices[index];
      if (total <= 0) return;
      const span = slice.end - slice.start;
      if (span < 0.36 || slice.value / total < 0.05) return;
      const mid = Charts.pieMidAngle(slice);
      ctx.fillStyle = pieLabelColor(identityColor(row.kind || "model", row.key));
      ctx.fillText(
        `${Charts.pieSharePercent(slice.value, total)}%`,
        cx + Math.cos(mid) * radius * 0.62,
        cy + Math.sin(mid) * radius * 0.62,
      );
    });

    renderChartLegend(
      legend,
      legendRows.map((row) => ({
        key: row.key,
        color: identityColor(row.kind || "model", row.key),
        value: row.value,
      })),
    );

    if (hidx >= 0) {
      if (Charts.tooltipFollowsPointer(state.inputMode, canvas.__mouse)) {
        const view = pieTooltipView(state.rows[hidx], total);
        showChartTooltip(canvas, canvas.__mouse.x, canvas.__mouse.y, view.content);
        announceChartData(canvas, Charts.liveRegionText(hidx, view.summary));
      } else {
        showPieTooltipFor(canvas, state, hidx);
      }
    } else {
      dismissChartHoverUi(canvas);
    }
  }

  function wireCharts() {
    if (!Charts) {
      noteChartsUnavailable();
      return;
    }
    for (const canvas of chartCanvases()) attachChartHover(canvas);
    syncChartSurface();
  }
  wireCharts();

  async function startAnalyticsPoll(epoch = tabEpoch) {
    stopAnalyticsPoll();
    await loadAnalytics({ fromPoll: true });
    if (epoch !== tabEpoch || activeTab !== "analytics") return;
    analyticsTimer = setInterval(() => loadAnalytics({ fromPoll: true }), 5000);
  }

  function stopAnalyticsPoll() {
    if (analyticsTimer) clearInterval(analyticsTimer);
    analyticsTimer = null;
  }

  function formatLogTime(ts) {
    if (!ts) return "";
    const date = new Date(Number(ts));
    if (Number.isNaN(date.getTime())) return String(ts);
    return date.toISOString().replace("T", " ").replace("Z", "");
  }

  function logKind(event, source) {
    if (source === "process") return (event.level || "info").toLowerCase();
    return (event.event || "event").toLowerCase();
  }

  function logMessage(event, source) {
    if (source === "process") {
      const target = event.target ? `${event.target} ` : "";
      return `${target}${event.message || ""}`.trim();
    }
    const parts = [event.event, event.id, event.model, event.provider_id, event.backend]
      .filter((value) => value != null && value !== "");
    return parts.join(" · ") || JSON.stringify(event);
  }

  function eventKey(event, source, index) {
    return `${source}:${event.ts || ""}:${event.id || ""}:${event.event || event.message || index}`;
  }

  function renderLogEvents(payload) {
    const viewer = $("#log-viewer");
    const source = payload.source || $("#log-source").value;
    const events = payload.events || [];
    if (!events.length) {
      const empty = document.createElement("div");
      empty.className = "log-empty";
      empty.textContent =
        source === "debug" && payload.enabled === false
          ? "Debug JSONL is disabled. Enable it in logging settings to capture request events."
          : payload.missing
            ? "No debug log file yet. Events appear after the next proxied request."
            : "No log events match the current filters.";
      viewer.replaceChildren(empty);
      return;
    }
    const follow = $("#log-follow")?.checked;
    const stickToBottom = follow && (viewer.scrollHeight - viewer.scrollTop - viewer.clientHeight < 40);
    viewer.innerHTML = "";
    events.forEach((event, index) => {
      const key = eventKey(event, source, index);
      const row = document.createElement("div");
      const kind = logKind(event, source);
      row.className = `log-row${logsExpanded.has(key) ? " open" : ""}`;
      const ts = document.createElement("span");
      ts.className = "log-ts";
      ts.textContent = formatLogTime(event.ts);
      const kindEl = document.createElement("span");
      const kindClass = String(kind).toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
      kindEl.classList.add("log-kind", kindClass || "unknown");
      kindEl.textContent = kind;
      const msg = document.createElement("span");
      msg.className = "log-msg";
      msg.textContent = logMessage(event, source);
      row.append(ts, kindEl, msg);
      if (logsExpanded.has(key)) {
        const pre = document.createElement("pre");
        pre.className = "log-json";
        pre.textContent = JSON.stringify(event, null, 2);
        row.append(pre);
      }
      row.addEventListener("click", () => {
        if (logsExpanded.has(key)) logsExpanded.delete(key);
        else logsExpanded.add(key);
        renderLogEvents(payload);
      });
      viewer.append(row);
    });
    if (stickToBottom) viewer.scrollTop = viewer.scrollHeight;
  }

  function applyLoggingChrome(settings) {
    const form = $("#logging-settings");
    if (!form) return;
    form.log_path.placeholder = settings.default_log_path || "codex-warp-debug.jsonl";
    form.max_log_mb.placeholder = String(settings.max_log_mb_effective ?? "");
    form.max_log_age_days.placeholder = String(settings.max_log_age_days_effective ?? "");
    form.tracing_filter.placeholder = settings.tracing_filter_wanted || settings.tracing_filter_effective || "codex_warp=debug";
    const hint = $("#logging-persist-hint");
    if (hint) hint.textContent = loggingHint(settings);
  }

  function applyLoggingFields(settings) {
    const form = $("#logging-settings");
    if (!form) return;
    loggingHydrating = true;
    try {
      form.enabled.checked = !!settings.enabled;
      form.log_path.value = settings.log_path || "";
      form.include_bodies.checked = !!settings.include_bodies;
      form.include_stream_bodies.checked = !!settings.include_stream_bodies;
      form.max_log_mb.value = settings.max_log_mb ?? "";
      form.max_log_age_days.value = settings.max_log_age_days ?? "";
      form.tracing_filter.value = settings.tracing_filter || "";
      loggingFormDirty = false;
    } finally {
      loggingHydrating = false;
    }
  }

  async function loadLoggingSettings({ hydrateFields = true } = {}) {
    const settings = await api("/logging");
    applyLoggingChrome(settings);
    if (hydrateFields && !loggingFormDirty) {
      applyLoggingFields(settings);
    }
    return settings;
  }

  function setLoggingFormHydrated(hydrated) {
    loggingSettingsHydrated = hydrated;
    const fields = $("#logging-settings-fields");
    if (fields) fields.disabled = !hydrated;
  }

  function tracingLagNote(settings) {
    if (settings.tracing_applied) return "";
    const effective = settings.tracing_filter_effective;
    return effective
      ? `Process logs still use ${effective}.`
      : "Process logs are not using the live tracing filter.";
  }

  function loggingReadyStatus(settings) {
    return tracingLagNote(settings) || "Ready";
  }

  function loggingHint(settings) {
    const base = settings.persist_available
      ? "Changes apply immediately. SQLite keeps them across restarts when the store is open. Command-line debug flags still win for that process."
      : "Changes apply immediately for this process. Persistence is unavailable (--no-webui-store), so they reset on restart.";
    const lag = tracingLagNote(settings);
    return lag ? `${base} ${lag}` : base;
  }

  function loggingSaveStatus(settings, remainingEdits = false) {
    const applied = settings.persisted
      ? "Logging settings applied"
      : settings.persist_available
        ? "Logging settings applied for this process; they could not be saved for restart"
        : "Logging settings applied for this process";
    const lag = tracingLagNote(settings);
    const remaining = remainingEdits ? "Unsaved edits remain." : "";
    return [applied, lag, remaining].filter(Boolean).join(" ");
  }

  async function loadLogs() {
    if (logsInFlight) {
      logsPending = true;
      return;
    }
    logsInFlight = true;
    try {
      const source = $("#log-source").value;
      const params = new URLSearchParams({ source, limit: "250" });
      const query = $("#log-query").value.trim();
      if (query) params.set("q", query);
      if (source === "process") {
        const level = $("#log-level").value;
        if (level) params.set("level", level);
      }
      const payload = await api(`/logging/events?${params.toString()}`);
      const meta = [];
      if (payload.path) meta.push(payload.path);
      if (payload.file_bytes) meta.push(`${payload.file_bytes} bytes`);
      if (payload.truncated) meta.push("showing a tail of a large file");
      $("#log-meta").textContent = meta.join(" · ");
      renderLogEvents(payload);
    } catch (e) {
      // Keep the footer for logging-settings state (including tracing lag).
      $("#log-meta").textContent = `Error: ${formatErrorMessage(e)}`;
    } finally {
      logsInFlight = false;
      if (logsPending && activeTab === "logs") {
        logsPending = false;
        loadLogs();
      } else {
        logsPending = false;
      }
    }
  }

  function syncProcessLevelControl() {
    const process = $("#log-source")?.value === "process";
    if ($("#log-level")) $("#log-level").disabled = !process;
  }

  function bumpLogsPollTimer() {
    if (!logsTimer) return;
    clearInterval(logsTimer);
    logsTimer = setInterval(loadLogs, 2500);
  }

  async function startLogsPoll({ epoch = tabEpoch } = {}) {
    stopLogsPoll();
    syncProcessLevelControl();
    // Disable only until the first successful hydrate so tab switches do not
    // interrupt in-progress edits.
    if (!loggingSettingsHydrated) setLoggingFormHydrated(false);
    try {
      const settings = await loadLoggingSettings({ hydrateFields: !loggingFormDirty });
      if (epoch !== tabEpoch || activeTab !== "logs") return settings;
      setLoggingFormHydrated(true);
      pollStatus(loggingReadyStatus(settings));
      loadLogs();
      logsTimer = setInterval(loadLogs, 2500);
      return settings;
    } catch (e) {
      if (epoch === tabEpoch && activeTab === "logs") {
        pollStatus(`Error: ${formatErrorMessage(e)}`);
      }
      throw e;
    }
  }

  function stopLogsPoll() {
    if (logsTimer) clearInterval(logsTimer);
    logsTimer = null;
  }

  $("#log-source")?.addEventListener("change", () => {
    syncProcessLevelControl();
    bumpLogsPollTimer();
    loadLogs();
  });
  $("#log-level")?.addEventListener("change", () => {
    bumpLogsPollTimer();
    loadLogs();
  });
  $("#log-query")?.addEventListener("input", () => {
    bumpLogsPollTimer();
    loadLogs();
  });
  $("#log-refresh")?.addEventListener("click", loadLogs);
  $("#logging-settings")?.addEventListener("input", () => {
    if (!loggingHydrating) loggingFormDirty = true;
  });
  $("#logging-settings")?.addEventListener("change", () => {
    if (!loggingHydrating) loggingFormDirty = true;
  });
  $("#logging-settings")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!loggingSettingsHydrated) {
      return;
    }
    const form = event.currentTarget;
    const tracingFilter = form.tracing_filter.value.trim();
    try {
      const maxLogMb = optionalPositiveInt(form.max_log_mb.value, "Max log size (MB)");
      const maxLogAgeDays = optionalPositiveInt(form.max_log_age_days.value, "Max log age (days)");
      status("Saving logging settings…");
      // Submitted values are the new baseline. Keystrokes during the PUT
      // re-dirty the form so success must not overwrite them from the response.
      loggingFormDirty = false;
      const saved = await api("/logging", {
        method: "PUT",
        body: JSON.stringify({
          enabled: form.enabled.checked,
          log_path: form.log_path.value.trim() || null,
          include_bodies: form.include_bodies.checked,
          include_stream_bodies: form.include_stream_bodies.checked,
          max_log_mb: maxLogMb,
          max_log_age_days: maxLogAgeDays,
          tracing_filter: tracingFilter ? tracingFilter : null,
        }),
      });
      applyLoggingChrome(saved);
      if (!loggingFormDirty) {
        applyLoggingFields(saved);
      }
      setLoggingFormHydrated(true);
      await loadLogs();
      status(loggingSaveStatus(saved, loggingFormDirty));
    } catch (e) {
      loggingFormDirty = true;
      try {
        await loadLoggingSettings({ hydrateFields: false });
        await loadLogs();
      } catch {
        /* still report the save error */
      }
      status(`Error: ${formatErrorMessage(e)}`);
    }
  });

  async function boot() {
    showTabPanel(tabFromLocation());
    status("Loading…");
    // Range and any already-present defaults can restore even when an
    // unrelated boot dependency fails. Provider loading retries restoration
    // as soon as its option inventory becomes available.
    restoreAnalyticsFilters();
    try {
      await Promise.all([
        loadProviders({ refreshRoutes: true, updateStatus: false }),
        loadProviderTemplates(),
      ]);
      bootComplete = true;
      await activateTabPolls(activeTab);
    } catch (e) {
      bootComplete = true;
      bootFooterHold = true;
      commitStatus(`Error: ${formatErrorMessage(e)}`, { remap: false });
      try {
        await activateTabPolls(activeTab);
      } catch {
        /* keep the boot error in the footer */
      }
    }
  }

  boot();
})();
