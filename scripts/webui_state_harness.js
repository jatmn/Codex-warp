#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../src/webui_static/app-main.js"), "utf8");
function between(start, end) {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from);
  assert(from >= 0 && to > from, "production markers must exist");
  return source.slice(from, to);
}

async function logRequests() {
  const controls = {
    "#log-source": { value: "process" }, "#log-query": { value: "" },
    "#log-level": { value: "info" }, "#log-meta": { textContent: "" },
  };
  const requests = [];
  const rendered = [];
  const context = vm.createContext({
    URLSearchParams,
    $: (id) => controls[id],
    api: (url) => new Promise((resolve, reject) => requests.push({ url, resolve, reject })),
    renderLogEvents: (payload) => rendered.push(payload),
    formatErrorMessage: (error) => error.message,
  });
  vm.runInContext(`
    let logsRequest = { phase: "idle" };
    let activeTab = "logs", tabEpoch = 1;
    ${between("async function loadLogs()", "function syncProcessLevelControl()")}
  `, context);
  const first = vm.runInContext("loadLogs()", context);
  controls["#log-query"].value = "new";
  await vm.runInContext("loadLogs()", context);
  requests[0].resolve({ events: ["old"], path: "old" });
  await first;
  assert.deepEqual(rendered, [], "an old-filter result must never render");
  assert.equal(requests.length, 2, "filter change must run one queued followup");
  requests[1].resolve({ events: ["new"], path: "new" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(rendered[0].path, "new");
  const staleError = vm.runInContext("loadLogs()", context);
  vm.runInContext('activeTab = "providers"; tabEpoch++;', context);
  requests[2].reject(new Error("old request failed"));
  await staleError;
  assert.equal(controls["#log-meta"].textContent, "new", "off-tab errors must not replace metadata");
}

function credentials() {
  const handlers = {};
  const apiKeyInput = { value: "", addEventListener: (event, fn) => { handlers[event] = fn; }, focus() {} };
  let clear;
  const context = vm.createContext({
    apiKeyInput, $: () => null,
    clearCredentialsBtn: { addEventListener: (_, fn) => { clear = fn; } },
  });
  vm.runInContext(between("function looksLikeEnvVarName(value)", '$("#btn-add-provider").addEventListener'), context);
  const patch = () => JSON.parse(vm.runInContext("JSON.stringify(credentialPatch())", context));
  vm.runInContext('setCredentialInput("", "sk-a••••zzzz", true, true)', context);
  assert.equal(apiKeyInput.readOnly, true);
  assert.deepEqual(patch(), { kind: "keep" });
  clear();
  assert.equal(apiKeyInput.readOnly, false);
  assert.deepEqual(patch(), { kind: "clear" });
  apiKeyInput.value = "replacement-secret";
  handlers.input(); handlers.blur();
  assert.deepEqual(patch(), { kind: "set", value: "replacement-secret" });
  handlers.focus();
  assert.equal(apiKeyInput.value, "replacement-secret", "focus must restore the draft, not its mask");
  apiKeyInput.value = "";
  handlers.input(); handlers.blur();
  assert.deepEqual(patch(), { kind: "clear" }, "empty replacement must retain the clear intent");
  vm.runInContext('setCredentialInput("OPENAI_API_KEY", "", true)', context);
  assert.deepEqual(patch(), { kind: "keep" });
  apiKeyInput.value = "OPENAI"; handlers.input();
  assert.equal(patch().kind, "invalid");
  apiKeyInput.value = "OPENAI_API_KEY"; handlers.input();
  assert.deepEqual(patch(), { kind: "keep" }, "reverting an env edit must retain the saved selector");
}

async function loggingEditor() {
  const handlers = {};
  const form = { addEventListener: (event, fn) => { handlers[event] = fn; } };
  for (const name of ["enabled", "log_path", "include_bodies", "include_stream_bodies", "max_log_mb", "max_log_age_days", "tracing_filter"]) {
    form[name] = { value: "", checked: false };
  }
  const fields = { disabled: true };
  const requests = [];
  const context = vm.createContext({
    $: (id) => id === "#logging-settings" ? form : id === "#logging-settings-fields" ? fields : null,
    api: (url, options) => new Promise((resolve, reject) => requests.push({ url, options, resolve, reject })),
    status() {}, loggingHint: () => "", loggingSaveStatus: () => "",
    formatErrorMessage: (error) => error.message,
    optionalPositiveInt: (value) => value ? Number(value) : null,
    loadLogs: async () => {},
  });
  vm.runInContext(`
    let tabEpoch = 0;
    ${between('let loggingFormPhase = "unhydrated"', "let bootComplete")}
    ${between("function applyLoggingChrome(settings)", "function tracingLagNote(settings)")}
    ${between('$("#logging-settings")?.addEventListener("input"', "async function boot()")}
  `, context);
  const submit = () => handlers.submit({ preventDefault() {}, currentTarget: form });
  await submit();
  assert.equal(requests.length, 0, "unhydrated values cannot be submitted");
  vm.runInContext('applyLoggingFields({log_path: "loaded"}); setLoggingFormHydrated(true)', context);
  form.log_path.value = "submitted"; handlers.input();
  const save = submit();
  assert.equal(JSON.parse(requests[0].options.body).log_path, "submitted");
  await submit();
  assert.equal(requests.length, 1, "duplicate submissions cannot race");
  const refresh = vm.runInContext("loadLoggingSettings()", context);
  requests[1].resolve({ log_path: "old-server-value" });
  await refresh;
  assert.equal(form.log_path.value, "submitted", "background hydrate cannot overwrite an in-flight save");
  form.log_path.value = "new-draft"; handlers.input();
  requests[0].resolve({ log_path: "submitted" });
  await save;
  assert.equal(form.log_path.value, "new-draft", "save response must preserve later edits");
  const failed = submit();
  requests[2].reject(new Error("save failed"));
  await new Promise((resolve) => setImmediate(resolve));
  requests[3].resolve({ log_path: "server-value" });
  await failed;
  assert.equal(form.log_path.value, "new-draft", "failure recovery must preserve the draft");
  const retry = submit();
  const delayedRefresh = vm.runInContext("loadLoggingSettings()", context);
  requests[4].resolve({ log_path: "normalized-saved-value" });
  await retry;
  requests[5].resolve({ log_path: "older-than-save" });
  await delayedRefresh;
  assert.equal(form.log_path.value, "normalized-saved-value", "untouched successful save hydrates its response");
}

Promise.resolve().then(credentials).then(logRequests).then(loggingEditor).then(() => console.log("webui-state-harness: ok")).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
