(() => {
  "use strict";

  const API = "/api";
  const TOKEN_KEY = "codex-warp-webui-token";
  function readStoredToken() {
    try { return sessionStorage.getItem(TOKEN_KEY) || ""; } catch { return ""; }
  }
  function storeToken(token) {
    try { sessionStorage.setItem(TOKEN_KEY, token); } catch { /* optional persistence */ }
  }
  function clearStoredToken() {
    try { sessionStorage.removeItem(TOKEN_KEY); } catch { /* optional persistence */ }
  }
  let managementToken = readStoredToken();
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
  let activeTab = "analytics";
  let bootComplete = false;
  let tabEpoch = 0;
  const expandedProviderIds = new Set();
  const VALID_TABS = new Set(["analytics", "providers", "logs"]);

  const $ = (sel) => document.querySelector(sel);
  const status = (msg) => { $("#status-line").textContent = msg; };

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
    return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths}</svg>`;
  }
  const ICONS = {
    chevron: svgIcon('<path d="M6 9l6 6 6-6"></path>'),
    trash: svgIcon(
      '<path d="M3 6h18"></path><path d="M8 6V4h8v2"></path><path d="M19 6l-1 14H6L5 6"></path><path d="M10 11v6M14 11v6"></path>',
    ),
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

  function esc(s) {
    const d = document.createElement("div");
    d.textContent = s ?? "";
    return d.innerHTML;
  }

  function tabFromLocation() {
    const hash = location.hash.replace(/^#/, "");
    return VALID_TABS.has(hash) ? hash : "analytics";
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
      await startLogsPoll({ updateFooter: true, epoch });
      return;
    }
    if (epoch === tabEpoch) status("Ready");
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
    fillAnalyticsFilters();
    if (refreshRoutes) {
      // Mutations refresh routes server-side. Initial discovery is best-effort
      // background enrichment and republishes the provider view when complete.
      void refreshModelRoutes().then(async (refreshed) => {
        if (!refreshed) return;
        try {
          providers = await api("/providers");
          renderProviders();
          fillAnalyticsFilters();
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
      title.innerHTML = `<strong>${esc(p.display_name)}</strong><span>${esc(p.id)} · ${esc(p.base_url)}</span>`;

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
          status(`Error: ${e.message}`);
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
      expandBtn.innerHTML = ICONS.chevron;
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
      editBtn.addEventListener("click", () => openProviderForm(p));

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
        } catch (e) { status(`Error: ${e.message}`); }
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
    container.innerHTML = "";
    const models = provider.models || [];
    if (!models.length) {
      container.innerHTML = "<p class='models-label'>Models</p><p class='muted'>No models.</p>";
      return;
    }
    const label = document.createElement("p");
    label.className = "models-label";
    label.textContent = "Models";
    container.append(label);
    for (const m of models) {
      const row = document.createElement("div");
      row.className = "model-row";
      const meta = document.createElement("div");
      meta.className = "model-meta";
      meta.innerHTML = `<strong>${esc(m.display_name || m.id)}</strong><small>${esc(m.id)}</small>`;
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
          status(`Error: ${e.message}`);
        }
      });
      sw.input.checked = m.enabled;
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn icon danger";
      del.innerHTML = ICONS.trash;
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
        } catch (e) { status(`Error: ${e.message}`); }
      });
      const actions = [sw.wrap];
      if (m.catalog) {
        const edit = document.createElement("button");
        edit.type = "button";
        edit.className = "btn small";
        edit.textContent = "Edit";
        edit.addEventListener("click", () => openModelForm(provider.id, m));
        actions.push(edit);
      }
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

  $("#btn-add-provider").addEventListener("click", () => openProviderForm());
  $("#provider-form-cancel").addEventListener("click", () => providerDialog.close());
  templateSelect.addEventListener("change", () => applySelectedTemplate());

  providerForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(providerForm);
    const id = String(fd.get("id") || "").trim();
    const mode = providerForm.dataset.mode || "create";
    const template = mode === "create"
      ? findTemplateByOptionValue(templateSelect.value)
      : null;
    const body = {
      name: String(fd.get("name") || "").trim() || null,
      base_url: String(fd.get("base_url") || "").trim(),
      api_key_env: String(fd.get("api_key_env") || "").trim() || null,
      auth_header: String(fd.get("auth_header") || "").trim() || "authorization",
      auth_scheme: String(fd.get("auth_scheme") || "").trim() || "Bearer",
      responses_path: String(fd.get("responses_path") || "").trim() || "/responses",
      chat_completions_path:
        String(fd.get("chat_completions_path") || "").trim() || "/chat/completions",
      models_path: String(fd.get("models_path") || "").trim() || "/models",
      model_catalog_only: providerForm.querySelector("[name=model_catalog_only]").checked,
      enabled: providerForm.querySelector("[name=enabled]")?.checked ?? true,
    };
    try {
      if (mode === "create") {
        const isCustom = !template || template.key === "custom";
        const payload = isCustom
          ? {
              template: "custom",
              id,
              ...body,
              model_catalog: selectedTemplateCatalog,
            }
          : {
              template: template.key,
              id: template.id,
              api_key_env: body.api_key_env,
              enabled: body.enabled,
            };
        await api("/providers", {
          method: "POST",
          body: JSON.stringify(payload),
        });
      } else {
        await api(`/providers/${encodeURIComponent(id)}`, {
          method: "PUT",
          body: JSON.stringify({
            name: body.name,
            base_url: body.base_url,
            api_key_env: body.api_key_env,
            auth_header: body.auth_header,
            auth_scheme: body.auth_scheme,
            responses_path: body.responses_path,
            chat_completions_path: body.chat_completions_path,
            models_path: body.models_path,
            model_catalog_only: body.model_catalog_only,
            enabled: body.enabled,
          }),
        });
      }
      providerDialog.close();
      await loadProviders({ refreshRoutes: false });
      status(mode === "create" ? `Provider ${id} created` : `Provider ${id} updated`);
    } catch (e) { status(`Error: ${e.message}`); }
  });

  function templateOptionValue(template) {
    return template.key;
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
      .slice(0, 8)
      .map((name) => esc(name));
    const more = catalog.length > 8 ? ` +${catalog.length - 8} more` : "";
    templateCatalogPreview.hidden = false;
    templateCatalogPreview.innerHTML =
      `<strong>${catalog.length} catalog model${catalog.length === 1 ? "" : "s"}</strong>` +
      `<div>${names.join(" · ")}${more}</div>`;
  }

  function setNamedTemplateMode(isNamed) {
    const identity = $("#provider-identity-fields");
    const advanced = $("#provider-advanced");
    const idInput = providerForm.querySelector("[name=id]");
    const baseUrlInput = providerForm.querySelector("[name=base_url]");
    identity.classList.toggle("template-locked", isNamed);
    advanced.hidden = isNamed;
    idInput.readOnly = isNamed;
    baseUrlInput.readOnly = isNamed;
    providerForm.querySelector("[name=name]").readOnly = isNamed;
    ["auth_header", "auth_scheme", "responses_path", "chat_completions_path", "models_path"]
      .forEach((name) => {
        providerForm.querySelector(`[name=${name}]`).readOnly = isNamed;
      });
    providerForm.querySelector("[name=model_catalog_only]").disabled = isNamed;
    if (isNamed) {
      idInput.removeAttribute("required");
      baseUrlInput.removeAttribute("required");
    } else {
      idInput.setAttribute("required", "required");
      baseUrlInput.setAttribute("required", "required");
    }
  }

  function applySelectedTemplate() {
    const template = findTemplateByOptionValue(templateSelect.value);
    const idInput = providerForm.querySelector("[name=id]");
    if (!template) {
      selectedTemplateCatalog = [];
      templateDescription.textContent = "";
      renderCatalogPreview([]);
      setNamedTemplateMode(false);
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
    providerForm.querySelector("[name=api_key_env]").value = template.api_key_env || "";
    providerForm.querySelector("[name=auth_header]").value =
      template.auth_header || "authorization";
    providerForm.querySelector("[name=auth_scheme]").value = template.auth_scheme || "Bearer";
    providerForm.querySelector("[name=responses_path]").value =
      template.responses_path || "/responses";
    providerForm.querySelector("[name=chat_completions_path]").value =
      template.chat_completions_path || "/chat/completions";
    providerForm.querySelector("[name=models_path]").value = template.models_path || "/models";
    providerForm.querySelector("[name=model_catalog_only]").checked = !!template.model_catalog_only;
    providerForm.querySelector("[name=enabled]").checked = true;
    setNamedTemplateMode(isNamed);
    renderCatalogPreview(selectedTemplateCatalog);
    if (!isNamed) {
      idInput.focus();
    } else {
      providerForm.querySelector("[name=api_key_env]").focus();
    }
  }

  function findTemplateForProvider(provider) {
    if (!provider) return null;
    return (
      providerTemplates.find((template) => template.id === provider.id) ||
      providerTemplates.find((template) => template.key === "custom")
    );
  }

  function openProviderForm(p = null) {
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
      templateSelect.value = matching
        ? templateOptionValue(matching)
        : templateOptionValue(
            providerTemplates.find((template) => template.key === "custom") ||
              providerTemplates[0],
          );
      templateDescription.textContent =
        matching?.description ||
        "This provider does not match a bundled example template.";
      templateCatalogPreview.hidden = true;
      enabledField.hidden = false;
      setNamedTemplateMode(false);
      $("#provider-advanced").hidden = false;
      idInput.value = p.id;
      idInput.readOnly = true;
      providerForm.querySelector("[name=name]").value = p.name || "";
      providerForm.querySelector("[name=base_url]").value = p.base_url || "";
      providerForm.querySelector("[name=api_key_env]").value = p.api_key_env || "";
      const apiKeyEnvInput = providerForm.querySelector("[name=api_key_env]");
      apiKeyEnvInput.readOnly = !p.managed;
      apiKeyEnvInput.title = p.managed
        ? ""
        : "TOML-backed providers manage api_key_env in TOML.";
      providerForm.querySelector("[name=auth_header]").value = p.auth_header || "authorization";
      providerForm.querySelector("[name=auth_scheme]").value = p.auth_scheme || "Bearer";
      providerForm.querySelector("[name=responses_path]").value = p.responses_path || "/responses";
      providerForm.querySelector("[name=chat_completions_path]").value =
        p.chat_completions_path || "/chat/completions";
      providerForm.querySelector("[name=models_path]").value = p.models_path || "/models";
      providerForm.querySelector("[name=model_catalog_only]").checked = !!p.model_catalog_only;
      providerForm.querySelector("[name=enabled]").checked = !!p.enabled;
    } else {
      providerForm.reset();
      providerForm.querySelector("[name=api_key_env]").readOnly = false;
      providerForm.querySelector("[name=api_key_env]").title = "";
      providerForm.dataset.mode = "create";
      $("#provider-form-title").textContent = "Add from example template";
      templateField.hidden = false;
      templateSelect.disabled = false;
      enabledField.hidden = false;
      populateTemplateSelect();
      const preferred =
        providerTemplates.find((template) => template.key === "openrouter") ||
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
  $("#model-form-cancel").addEventListener("click", () => modelDialog.close());
  modelForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(modelForm);
    const providerId = fd.get("provider_id");
    const body = {
      id: fd.get("id").trim(),
      upstream_id: fd.get("upstream_id")?.trim() || null,
      display_name: fd.get("display_name")?.trim() || null,
      description: fd.get("description")?.trim() || null,
      enabled: editingModel?.enabled ?? true,
    };
    const mode = modelForm.dataset.mode || "create";
    try {
      if (mode === "create") {
        await api(`/providers/${encodeURIComponent(providerId)}/models`, {
          method: "POST",
          body: JSON.stringify(body),
        });
      } else {
        await api(
          `/providers/${encodeURIComponent(providerId)}/models/${encodeURIComponent(body.id)}`,
          { method: "PUT", body: JSON.stringify(body) },
        );
      }
      modelDialog.close();
      await loadProviders({ refreshRoutes: false });
    } catch (e) { status(`Error: ${e.message}`); }
  });

  function openModelForm(providerId, m = null) {
    modelForm.reset();
    editingModel = m;
    modelForm.querySelector("[name=provider_id]").value = providerId;
    const idInput = modelForm.querySelector("[name=id]");
    if (m) {
      modelForm.dataset.mode = "edit";
      $("#model-form-title").textContent = "Edit model";
      idInput.value = m.id;
      idInput.readOnly = true;
      modelForm.querySelector("[name=upstream_id]").value = m.upstream_id || "";
      modelForm.querySelector("[name=display_name]").value = m.display_name || "";
      modelForm.querySelector("[name=description]").value = m.description || "";
    } else {
      modelForm.dataset.mode = "create";
      $("#model-form-title").textContent = "Add model";
      idInput.readOnly = false;
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

  let analyticsPending = { queued: false };

  $("#analytics-provider").addEventListener("change", () => {
    $("#analytics-model").value = "";
    fillAnalyticsFilters();
    loadAnalytics();
  });
  $("#analytics-range").addEventListener("change", loadAnalytics);
  $("#analytics-model").addEventListener("change", loadAnalytics);

  async function loadAnalytics() {
    if (analyticsInFlight) {
      analyticsPending.queued = true;
      return;
    }
    analyticsInFlight = true;
    analyticsPending.queued = false;
    const range = $("#analytics-range").value;
    const provider = $("#analytics-provider").value;
    const model = $("#analytics-model").value;
    const qs = new URLSearchParams({ range });
    if (provider) qs.set("provider", provider);
    if (model) qs.set("model", model);
    try {
      const data = await api(`/analytics?${qs}`);
      // Preserve provider identities from retained usage even after their live
      // configuration is removed. Filtered responses omit this breakdown.
      if (!provider) {
        analyticsProviderIds = (data.by_provider || [])
          .map((row) => row.key)
          .filter(Boolean);
      }
      // A model-filtered response deliberately omits the by-model breakdown.
      // Keep the independent option inventory so the active filter survives
      // this response and subsequent polling.
      if (!model) {
        analyticsModelIds = (data.by_model || [])
          .map((row) => row.key)
          .filter(Boolean);
        analyticsModelProvider = provider;
      }
      fillAnalyticsFilters();
      analyticsSnapshot = {
        data,
        range,
        barTitle: model
          ? `${model} over time`
          : provider
            ? `${provider} over time`
            : "Usage over time",
      };
      renderAnalyticsPresentation();
      if (activeTab === "analytics") status("Analytics updated");
    } catch (e) {
      if (activeTab === "analytics") status(`Analytics error: ${e.message}`);
    } finally {
      analyticsInFlight = false;
      if (analyticsPending.queued && activeTab === "analytics") {
        loadAnalytics();
      }
    }
  }

  function formatBucketLabel(ms, range) {
    if (!ms) return "";
    const d = new Date(ms);
    if (range === "yearly" || range === "30d" || range === "week") {
      return `${d.getUTCMonth() + 1}/${d.getUTCDate()}`;
    }
    return `${String(d.getUTCHours()).padStart(2, "0")}:${String(d.getUTCMinutes()).padStart(2, "0")}`;
  }

  function renderAnalyticsPresentation() {
    if (!analyticsSnapshot) return;
    const { data, range, barTitle } = analyticsSnapshot;
    renderAnalyticsCards(data);
    const series = data.series || [];
    drawLineChart($("#chart-line"), series);
    // Bar chart shows the same time series as bars so usage-over-time is visible
    // in both chart styles; breakdowns remain available via provider/model filters.
    $("#chart-bar-title").textContent = barTitle;
    drawBarChart(
      $("#chart-bar"),
      series.map((point) => ({
        key: formatBucketLabel(point.ts, range),
        total_tokens: point.total_tokens || 0,
        prompts: point.prompts || 0,
        sessions: point.sessions || 0,
      })),
    );
  }

  window.addEventListener("codex-warp-theme-change", () => renderAnalyticsPresentation());

  function renderAnalyticsCards(d) {
    const cards = $("#analytics-cards");
    const items = [
      ["Prompts", d.prompts],
      ["Sessions", d.sessions],
      ["Input tokens", d.input_tokens],
      ["Output tokens", d.output_tokens],
      ["Total tokens", d.total_tokens],
      ["Cached", d.cached_tokens],
      ["Reasoning", d.reasoning_tokens],
    ];
    cards.innerHTML = items.map(([label, val]) =>
      `<div class="card"><label>${esc(label)}</label><strong>${Number(val || 0).toLocaleString()}</strong></div>`
    ).join("");
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
      tokens: cssThemeColor("--chart-tokens", "#0f766e"),
      prompts: cssThemeColor("--chart-prompts", "#d97706"),
      sessions: cssThemeColor("--chart-sessions", "#16a34a"),
      bar: cssThemeColor("--chart-tokens", "#0f766e"),
    };
  }

  function drawLineChart(canvas, series) {
    const ctx = canvas.getContext("2d");
    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);
    if (!series.length) return;
    const pad = 28;
    const promptVals = series.map((p) => p.prompts || 0);
    const sessionVals = series.map((p) => p.sessions || 0);
    const tokenVals = series.map((p) => p.total_tokens || 0);
    const max = Math.max(1, ...promptVals, ...sessionVals, ...tokenVals);
    const step = (w - pad * 2) / Math.max(1, series.length - 1);
    const colors = chartColors();

    function strokeSeries(vals, color) {
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.beginPath();
      vals.forEach((val, i) => {
        const x = pad + i * step;
        const y = h - pad - (val / max) * (h - pad * 2);
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      });
      ctx.stroke();
    }

    strokeSeries(tokenVals, colors.tokens);
    strokeSeries(promptVals, colors.prompts);
    strokeSeries(sessionVals, colors.sessions);

    ctx.fillStyle = colors.muted;
    ctx.font = "11px system-ui";
    ctx.fillText("0", 4, h - pad);
    ctx.fillText(String(max), 4, pad + 4);
    ctx.fillStyle = colors.tokens;
    ctx.fillText("tokens", w - 60, pad + 4);
    ctx.fillStyle = colors.prompts;
    ctx.fillText("prompts", w - 60, pad + 18);
    ctx.fillStyle = colors.sessions;
    ctx.fillText("sessions", w - 60, pad + 32);
  }

  function drawBarChart(canvas, rows) {
    const ctx = canvas.getContext("2d");
    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);
    if (!rows.length) return;
    const pad = 28;
    const barGap = 6;
    const max = Math.max(1, ...rows.map((r) => r.total_tokens || 0));
    const barW = (w - pad * 2 - barGap * (rows.length - 1)) / rows.length;
    const colors = chartColors();
    rows.forEach((r, i) => {
      const val = r.total_tokens || 0;
      const barH = (val / max) * (h - pad * 2);
      const x = pad + i * (barW + barGap);
      const y = h - pad - barH;
      ctx.fillStyle = colors.bar;
      ctx.fillRect(x, y, barW, barH);
      ctx.fillStyle = colors.muted;
      ctx.font = "10px system-ui";
      const label = (r.key || "").slice(0, 8);
      ctx.fillText(label, x, h - 8);
    });
  }

  async function startAnalyticsPoll(epoch = tabEpoch) {
    stopAnalyticsPoll();
    await loadAnalytics();
    if (epoch !== tabEpoch || activeTab !== "analytics") return;
    analyticsTimer = setInterval(loadAnalytics, 5000);
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
      viewer.innerHTML = `<div class="log-empty">${
        source === "debug" && payload.enabled === false
          ? "Debug JSONL is disabled. Enable it in logging settings to capture request events."
          : payload.missing
            ? "No debug log file yet. Events appear after the next proxied request."
            : "No log events match the current filters."
      }</div>`;
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
      row.innerHTML = `<span class="log-ts">${esc(formatLogTime(event.ts))}</span><span class="log-kind ${esc(kind)}">${esc(kind)}</span><span class="log-msg">${esc(logMessage(event, source))}</span>`;
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

  async function loadLoggingSettings() {
    const settings = await api("/logging");
    const form = $("#logging-settings");
    if (!form) return settings;
    form.enabled.checked = !!settings.enabled;
    form.log_path.value = settings.log_path || settings.default_log_path || "";
    form.include_bodies.checked = !!settings.include_bodies;
    form.include_stream_bodies.checked = !!settings.include_stream_bodies;
    form.max_log_mb.value = settings.max_log_mb ?? "";
    form.max_log_age_days.value = settings.max_log_age_days ?? "";
    form.tracing_filter.value = settings.tracing_filter || "";
    form.tracing_filter.placeholder = settings.tracing_filter_wanted || settings.tracing_filter_effective || "codex_warp=debug";
    const hint = $("#logging-persist-hint");
    if (hint) hint.textContent = loggingHint(settings);
    return settings;
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

  function loggingSaveStatus(settings) {
    const applied = settings.persisted
      ? "Logging settings applied"
      : settings.persist_available
        ? "Logging settings applied for this process; they could not be saved for restart"
        : "Logging settings applied for this process";
    const lag = tracingLagNote(settings);
    return lag ? `${applied} ${lag}` : applied;
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
      $("#log-meta").textContent = `Error: ${e.message}`;
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

  async function startLogsPoll({ updateFooter = false, epoch = tabEpoch } = {}) {
    stopLogsPoll();
    syncProcessLevelControl();
    try {
      const settings = await loadLoggingSettings();
      if (epoch !== tabEpoch || activeTab !== "logs") return settings;
      if (updateFooter) status(loggingReadyStatus(settings));
      loadLogs();
      logsTimer = setInterval(loadLogs, 2500);
      return settings;
    } catch (e) {
      if (epoch === tabEpoch && activeTab === "logs" && updateFooter) {
        status(`Error: ${e.message}`);
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
  $("#logging-settings")?.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.currentTarget;
    const tracingFilter = form.tracing_filter.value.trim();
    try {
      status("Saving logging settings…");
      const saved = await api("/logging", {
        method: "PUT",
        body: JSON.stringify({
          enabled: form.enabled.checked,
          log_path: form.log_path.value.trim() || null,
          include_bodies: form.include_bodies.checked,
          include_stream_bodies: form.include_stream_bodies.checked,
          max_log_mb: form.max_log_mb.value ? Number(form.max_log_mb.value) : null,
          max_log_age_days: form.max_log_age_days.value ? Number(form.max_log_age_days.value) : null,
          tracing_filter: tracingFilter ? tracingFilter : null,
        }),
      });
      await loadLoggingSettings();
      await loadLogs();
      status(loggingSaveStatus(saved));
    } catch (e) {
      try {
        await loadLoggingSettings();
        await loadLogs();
      } catch {
        /* still report the save error */
      }
      status(`Error: ${e.message}`);
    }
  });

  async function boot() {
    showTabPanel(tabFromLocation());
    status("Loading…");
    try {
      await Promise.all([
        loadProviders({ refreshRoutes: true, updateStatus: false }),
        loadProviderTemplates(),
      ]);
      bootComplete = true;
      await activateTabPolls(activeTab);
    } catch (e) {
      bootComplete = true;
      status(`Error: ${e.message}`);
    }
  }

  boot();
})();
