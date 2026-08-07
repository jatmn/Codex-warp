(() => {
  "use strict";

  const API = "/api";
  let providers = [];
  let analyticsModelIds = [];
  let analyticsTimer = null;
  let analyticsInFlight = false;
  let activeTab = "providers";

  const $ = (sel) => document.querySelector(sel);
  const status = (msg) => { $("#status-line").textContent = msg; };

  async function api(path, opts = {}) {
    const res = await fetch(API + path, {
      headers: { "Content-Type": "application/json", ...(opts.headers || {}) },
      ...opts,
    });
    const text = await res.text();
    let data = null;
    try { data = text ? JSON.parse(text) : null; } catch { data = { error: text }; }
    if (!res.ok) throw new Error(data?.error || res.statusText);
    return data;
  }

  function esc(s) {
    const d = document.createElement("div");
    d.textContent = s ?? "";
    return d.innerHTML;
  }

  function switchTab(name) {
    activeTab = name;
    document.querySelectorAll(".tab").forEach((b) => {
      b.classList.toggle("active", b.dataset.tab === name);
    });
    document.querySelectorAll(".panel").forEach((p) => {
      const on = p.id === `panel-${name}`;
      p.classList.toggle("active", on);
      p.hidden = !on;
    });
    if (name === "analytics") startAnalyticsPoll();
    else stopAnalyticsPoll();
  }

  document.querySelectorAll(".tab").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.dataset.tab));
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
    try {
      await fetch("/v1/models");
    } catch {
      // Best-effort: populate server model_routes for discovered upstream models.
    }
  }

  async function loadProviders() {
    status("Loading providers…");
    try {
      await refreshModelRoutes();
      providers = await api("/providers");
      renderProviders();
      fillAnalyticsFilters();
      status("Ready");
    } catch (e) {
      status(`Error: ${e.message}`);
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
          await refreshModelRoutes();
          await loadProviders();
          status(`${p.id} ${enabled ? "enabled" : "disabled"}`);
        } catch (e) {
          sw.input.checked = !enabled;
          status(`Error: ${e.message}`);
        }
      });
      sw.input.checked = p.enabled;

      const expandBtn = document.createElement("button");
      expandBtn.type = "button";
      expandBtn.className = "btn small";
      expandBtn.textContent = "Models";
      const models = document.createElement("div");
      models.className = "models collapsed";
      expandBtn.addEventListener("click", () => {
        models.classList.toggle("collapsed");
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
          await loadProviders();
        } catch (e) { status(`Error: ${e.message}`); }
      });

      const addModelBtn = document.createElement("button");
      addModelBtn.type = "button";
      addModelBtn.className = "btn small";
      addModelBtn.textContent = "Add model";
      addModelBtn.addEventListener("click", () => openModelForm(p.id));

      const actions = document.createElement("div");
      actions.className = "provider-actions";
      actions.append(expandBtn, editBtn, addModelBtn, delBtn);

      head.append(title, sw.wrap, actions);
      renderModels(p, models);
      card.append(head, models);
      list.append(card);
    }
  }

  function renderModels(provider, container) {
    container.innerHTML = "";
    const models = provider.models || [];
    if (!models.length) {
      container.innerHTML = "<p class='muted'>No models.</p>";
      return;
    }
    for (const m of models) {
      const row = document.createElement("div");
      row.className = "model-row";
      const meta = document.createElement("div");
      meta.className = "model-meta";
      meta.innerHTML = `<strong>${esc(m.display_name || m.id)}</strong><small>${esc(m.id)}</small>`;
      const sw = toggleSwitch(async (enabled) => {
        try {
          await api(
            `/providers/${encodeURIComponent(provider.id)}/models/enabled/${encodeURIComponent(m.id)}`,
            { method: "POST", body: JSON.stringify({ enabled }) },
          );
          m.enabled = enabled;
        } catch (e) {
          sw.input.checked = !enabled;
          status(`Error: ${e.message}`);
        }
      });
      sw.input.checked = m.enabled;
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn small danger";
      del.textContent = "Remove";
      del.addEventListener("click", async () => {
        if (!confirm(`Remove model ${m.id}?`)) return;
        try {
          await api(
            `/providers/${encodeURIComponent(provider.id)}/models/${encodeURIComponent(m.id)}`,
            { method: "DELETE" },
          );
          await loadProviders();
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
  $("#btn-add-provider").addEventListener("click", () => openProviderForm());
  $("#provider-form-cancel").addEventListener("click", () => providerDialog.close());
  providerForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(providerForm);
    const id = fd.get("id").trim();
    const body = {
      name: fd.get("name")?.trim() || null,
      base_url: fd.get("base_url").trim(),
      api_key_env: fd.get("api_key_env")?.trim() || null,
    };
    const mode = providerForm.dataset.mode || "create";
    try {
      if (mode === "create") {
        await api("/providers", { method: "POST", body: JSON.stringify({ id, ...body }) });
      } else {
        await api(`/providers/${encodeURIComponent(id)}`, {
          method: "PUT",
          body: JSON.stringify(body),
        });
      }
      providerDialog.close();
      await loadProviders();
    } catch (e) { status(`Error: ${e.message}`); }
  });

  function openProviderForm(p = null) {
    providerForm.reset();
    const idInput = providerForm.querySelector("[name=id]");
    if (p) {
      providerForm.dataset.mode = "edit";
      $("#provider-form-title").textContent = "Edit provider";
      idInput.value = p.id;
      idInput.readOnly = true;
      providerForm.querySelector("[name=name]").value = p.name || "";
      providerForm.querySelector("[name=base_url]").value = p.base_url || "";
      providerForm.querySelector("[name=api_key_env]").value = p.api_key_env || "";
    } else {
      providerForm.dataset.mode = "create";
      $("#provider-form-title").textContent = "Add provider";
      idInput.readOnly = false;
    }
    providerDialog.showModal();
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
      await loadProviders();
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
    for (const id of analyticsModelIds) {
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
      analyticsModelIds = (data.by_model || [])
        .map((row) => row.key)
        .filter(Boolean);
      fillAnalyticsFilters();
      renderAnalyticsCards(data);
      const series = data.series || [];
      drawLineChart($("#chart-line"), series);
      // Bar chart shows the same time series as bars so usage-over-time is visible
      // in both chart styles; breakdowns remain available via provider/model filters.
      $("#chart-bar-title").textContent = model
        ? `${model} over time`
        : provider
          ? `${provider} over time`
          : "Usage over time";
      drawBarChart(
        $("#chart-bar"),
        series.map((point) => ({
          key: formatBucketLabel(point.ts, range),
          total_tokens: point.total_tokens || 0,
          prompts: point.prompts || 0,
          sessions: point.sessions || 0,
        })),
      );
      status("Analytics updated");
    } catch (e) {
      status(`Analytics error: ${e.message}`);
    } finally {
      analyticsInFlight = false;
      if (analyticsPending.queued) {
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

    strokeSeries(tokenVals, "#3d9cdb");
    strokeSeries(promptVals, "#e8a838");
    strokeSeries(sessionVals, "#6bc96b");

    ctx.fillStyle = "#8b98a8";
    ctx.font = "11px system-ui";
    ctx.fillText("0", 4, h - pad);
    ctx.fillText(String(max), 4, pad + 4);
    ctx.fillStyle = "#3d9cdb";
    ctx.fillText("tokens", w - 60, pad + 4);
    ctx.fillStyle = "#e8a838";
    ctx.fillText("prompts", w - 60, pad + 18);
    ctx.fillStyle = "#6bc96b";
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
    rows.forEach((r, i) => {
      const val = r.total_tokens || 0;
      const barH = (val / max) * (h - pad * 2);
      const x = pad + i * (barW + barGap);
      const y = h - pad - barH;
      ctx.fillStyle = "#2a6f9e";
      ctx.fillRect(x, y, barW, barH);
      ctx.fillStyle = "#8b98a8";
      ctx.font = "10px system-ui";
      const label = (r.key || "").slice(0, 8);
      ctx.fillText(label, x, h - 8);
    });
  }

  function startAnalyticsPoll() {
    stopAnalyticsPoll();
    loadAnalytics();
    analyticsTimer = setInterval(loadAnalytics, 5000);
  }

  function stopAnalyticsPoll() {
    if (analyticsTimer) clearInterval(analyticsTimer);
    analyticsTimer = null;
  }

  loadProviders();
})();
