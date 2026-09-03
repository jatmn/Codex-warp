# Configuration Guide

`codex-warp.toml` is always loaded first. Every `--config` file is merged on
top of it, so provider profiles can stay small and provider-specific.

## Table Of Contents

- [Baseline Includes](#baseline-includes)
- [Provider Profiles](#provider-profiles)
- [Custom Headers](#custom-headers)
- [Model Metadata](#model-metadata)
- [Request Morphs](#request-morphs)
- [Continue Guard](#continue-guard)
- [Tool Approval Policy](#tool-approval-policy)
- [Web UI And Analytics](#web-ui-and-analytics)
- [Debug Logging](#debug-logging)

## Baseline Includes

The baseline config does not enable an upstream provider by default, but it does
load model-family catalogs and bundled tool-policy rules:

```toml
[config]
# Optional provider profiles:
# include = [
#   "configs/clinepass.toml",
#   "configs/hicap.toml",
#   "configs/moonshot-kimicode.toml",
#   "configs/opencode-go.toml",
#   "configs/xiaomi-token-plan.toml",
#   "configs/openai-compatible.toml",
# ]

model_family_include = [
  "configs/model-families/deepseek.toml",
  "configs/model-families/minimax.toml",
  "configs/model-families/moonshot-ai.toml",
  "configs/model-families/qwen.toml",
  "configs/model-families/x-ai.toml",
  "configs/model-families/xiaomi.toml",
  "configs/model-families/z-ai.toml",
]

tool_policy_include = [
  "configs/tool-policies/github.toml",
]
```

Included paths are resolved relative to the config file that declares them. The
bundled tool-policy rules are inert until `[tool_policy].enabled = true`.

## Provider Profiles

Primary provider:

```toml
[provider]
name = "Provider"
base_url = "https://provider.example/v1"
api_key_env = "PROVIDER_API_KEY"
auth_header = "authorization"
auth_scheme = "Bearer"
responses_path = "/responses"
chat_completions_path = "/chat/completions"
models_path = "/models"
model_catalog_only = false
```

Named providers:

```toml
[providers.provider_a]
name = "Provider A"
base_url = "https://provider-a.example/v1"
api_key_env = "PROVIDER_A_API_KEY"

[providers.provider_b]
name = "Provider B"
base_url = "https://provider-b.example/v1"
api_key_env = "PROVIDER_B_API_KEY"

[[providers.provider_b.model_catalog]]
id = "provider-b/model-a"
upstream_id = "model-a"
display_name = "Provider B Model A"
```

When Codex asks for `/v1/models`, Codex Warp merges the catalogs it can load and
remembers which provider reported each model. The response is grouped by gateway
and each model display name is prefixed with the provider `name`. Later
`/v1/responses` requests are routed by the selected `model`. If duplicate model
slugs exist, the first
provider to report that slug wins the route.

If a provider does not expose a usable `/models` route, add local
`model_catalog` entries to the provider profile. Codex Warp normalizes those
entries the same way it normalizes upstream model catalogs.

Set `model_catalog_only = true` when a provider exposes a `/models` route that
includes models this profile cannot safely route. `upstream_id` lets the Codex
model slug stay gateway-namespaced while sending a different model id upstream.

## Custom Headers

Some providers require extra headers:

```toml
[providers.manual.headers]
"HTTP-Referer" = "https://example.local"
"X-Title" = "Codex Warp"
```

Codex Warp attaches [OpenRouter app attribution](../README.md#openrouter-app-attribution)
headers (`HTTP-Referer`, `X-OpenRouter-Title`, `X-Title`, and
`X-OpenRouter-Categories`) only when the configured destination host is
`openrouter.ai` or a subdomain of it, including OpenRouter's regional API
hosts. Set any of those names under `[provider.headers]` or
`[providers.<id>.headers]` to override the automatic values for an OpenRouter
gateway. Traffic sent to another gateway is not reported to OpenRouter.

Codex Warp always sends its own `User-Agent` as `codex-warp/<version>` to
upstream providers. Configured `User-Agent` values are ignored so provider logs
can identify the proxy consistently.

## Model Metadata

Provider profiles can fill or override catalog metadata:

```toml
[provider.model_metadata.defaults]
context_window = 1000000
default_reasoning_level = "medium"
supported_reasoning_levels = ["low", "medium", "high"]

[provider.model_metadata.overrides."provider-model"]
input_modalities = ["text", "image"]
```

Codex Warp also picks up common upstream `/models` fields when providers expose
them, including:

- `context_window`
- `context_length`
- `max_context_length`
- `input_modalities`
- `modalities`
- `supports_vision`
- `supports_parallel_tool_calls`
- `supports_search_tool`
- `supported_reasoning_levels`

Precedence is:

1. synthetic Codex defaults
2. upstream `/models` fields
3. matching model-family metadata, in priority order
4. provider defaults
5. exact provider model overrides
6. explicit reasoning fields on a provider `model_catalog` entry

A catalog entry can override the modes for one routable model, including a
live upstream model selected through `upstream_id`:

```toml
[[providers.provider_b.model_catalog]]
id = "provider-b/model-a"
upstream_id = "model-a"
supported_reasoning_levels = ["low", "high", "max"]
default_reasoning_level = "high"
```

Both reasoning fields are optional. Omitting them inherits the normalized
upstream, model-family, and provider metadata. The default must be one of the
effective supported levels. An explicit `supported_reasoning_levels` list
selects which modes are advertised; matching upstream level objects keep their
descriptions and other per-level metadata, and newly added modes synthesize
`{effort, description}` objects.

## Request Morphs

The main transform knobs are:

- `transform.backend`: `open_ai_chat` translates `/responses` to
  `/chat/completions`; `responses` forwards to upstream `/responses`.
- `transform.chat_request_morphs`: translates Codex/Responses fields into
  chat-completions provider fields.
- `transform.responses_request_morphs`: translates native Responses requests
  before forwarding.
- `transform.remove_chat_request_morphs` /
  `transform.remove_responses_request_morphs`: removes inherited morphs before
  appending provider- or model-specific replacements.
- `transform.unsupported_tool_types`: tool types to rewrite or remove, for
  example `custom`.
- `transform.unsupported_tool_strategy`: `drop`, `as_function`, or
  `passthrough`.
- `transform.request_stream_options_include_usage`: defaults to `false` because
  some OpenAI-compatible gateways reject `stream_options`. Set it to `true` for
  a provider that documents `stream_options.include_usage`; requests that do
  not already include `stream_options` then receive
  `stream_options.include_usage = true` for local token analytics.

- `providers.<id>.request_stream_options_include_usage`: optional provider-level
  override for the same stream-usage injection. When set, it wins over the
  baseline and provider `transform` value after model-family patches are
  applied. Use this for managed Web UI providers (or TOML providers that should
  keep the shared baseline morphs) when the gateway only emits chat-stream
  token usage if `stream_options.include_usage` is true. Leave unset to inherit
  the transform default.
- `transform.preserve_native_agent_messages`: defaults to `false`, translating
  Codex-only `agent_message` history into standard user messages for compatible
  Responses gateways. Converted messages wait for outstanding call/output
  batches instead of inserting a user message between a call and its result.
  Set it to `true` only when the native Responses provider
  explicitly accepts Codex `agent_message` items and their encrypted content.

Supported request morph kinds:

| Kind | Meaning |
| --- | --- |
| `copy` | Copy one request path to another. |
| `rename` | Copy to a new path and remove the original for native Responses morphs. |
| `drop` | Intentionally discard a request path. |
| `text_format` | Convert Responses `text.format` JSON schema to chat `response_format`. |
| `thinking_type` | Convert `reasoning.effort` to provider `thinking.type`. |
| `static_string` | Set a fixed string value such as `thinking.keep = "all"`. |

Example:

```toml
[[model_families.example_model.transform.remove_chat_request_morphs]]
from = "reasoning.effort"
to = "reasoning_effort"
kind = "rename"

[[model_families.example_model.transform.append_chat_request_morphs]]
from = "reasoning.effort"
to = "thinking.type"
kind = "thinking_type"
```

The default unsupported-tool behavior is `as_function`, which keeps Codex
freeform tools such as `apply_patch` visible to chat-completions providers as a
function with a string `input`.

## Continue Guard

Continue guard is Codex Warp's safety rail for providers that prematurely end a
turn while the model is still narrating work it intends to do next. This can
happen with chat-completions gateways during long Codex tasks: the provider
returns `finish_reason = "stop"` with assistant text such as `Now let me
check...`, but no tool call follows and Codex closes the turn.

When the guard is active, Codex Warp converts that specific premature-stop shape
into a Responses `response.completed` event with `end_turn = false`. Codex
already treats that as a request to immediately sample a follow-up turn, so the
provider gets another chance to issue the tool call or continue the task without
the user manually prompting it.

```toml
[continue_guard]
enabled = true
mode = "end_turn_false"
max_followups = 1
```

The guard is **enabled by default** with `mode = "end_turn_false"` and
`max_followups = 1`, and it applies to every chat-completions provider routed
through Warp. You only need to set `enabled = false` if a particular gateway
never exhibits the premature-stop shape.

Existing installs that still ship a local `codex-warp.toml` from before the
default-on change may keep `enabled = false` / `mode = "observe"`. That stale
operator override fully disables auto-continue even though the binary defaults
are on. Align the runtime file with the block above, or pass `--continue-guard`
(and `--continue-guard-mode end_turn_false` if the file still has
`mode = "observe"`) and restart Warp; startup logs a warning when the loaded
config leaves the guard disabled.

The guard also treats unresolved sub-agent work as mid-task: when the request
history still contains `spawn_agent` (or collapsed `spawn`) calls that later
`wait_agent` / `wait_threads` / `wait` targets have not covered, a text-only
`stop` forces a follow-up so the parent does not end the turn while a child is
still running. A wait with an explicit `targets` / `thread_ids` / `agent_ids`
list resolves the unique matching children (duplicate IDs count once). While no
spawn output has named a child, a wait still falls back to that unique target
count. After a spawn output names a child, IDs that do not match a named spawn
output do not clear other outstanding children. A wait with no target list
acknowledges the whole outstanding wave. A wait whose arguments cannot be
parsed, or whose target field is present but is not a list of IDs, does not
acknowledge any children. Phrases
like `Let me wait for the agent`, `Now let me get the subagent findings by
resuming it`, and `Let me verify it end-to-end` continue; bare `I'll wait` and
`I'll get back to you` still end the turn.

Modes:

- `observe`: log `continue_guard` debug events for suspected premature stops,
  but leave `end_turn = true`. Use this when diagnosing whether a provider is
  hitting the pattern.
- `end_turn_false`: for suspected premature stops, emit
  `response.completed.response.end_turn = false` so Codex continues the turn.
  This is the default.

The guard applies to chat-completions responses, both SSE streams and
non-stream JSON, that finish with
`finish_reason = "stop"` (text-only JSON completions that omit
`finish_reason`, or send an empty `finish_reason`, are treated the same), emit no tool call, and end with continuation phrasing
(`let me` / `I'll` / `I need to` / `I should` when the next action is a known
work verb — including sub-agent resume verbs such as `get` / `resume` /
`collect` / `wait for <work>` — or an unlisted verb with a concrete object such
as `I'll clone the repo` / `I'll add tests`, including hyphenated repeats such
as `re-audit`;
`then` / `next` only when the next action is a known work verb, so
`Then run the tests` continues but `Next I need a decision from you` does not;
or a dangling `:`/`...` whose last sentence still talks about unfinished
speaker work, not a delivery frame such as `Here is a summary of remaining
work:`). Status copulas such as `This is still pending:` and bare unfinished
headers such as `Tasks remaining:` and clause remaining headers
(`Remaining tasks:`, `The remaining items:`, `All remaining tasks:`,
`Incomplete remaining tasks:`, `Complete remaining tasks:`,
`Summary, remaining tasks:`) still continue. Attributive remaining
inside an `and`-coordinated phrase (`Summary and remaining tasks:`) stays
`end_turn = true` because remaining is a modifier there, not a header or
predicate. Locative copulas (`Here are the remaining items:`,
`Below are remaining tasks:`, `Above are the remaining steps:`,
`Following are remaining tasks:`) stay delivery even when remaining appears later.
Remaining subjects whose copular predicate is completion
(`Remaining work is complete:`, `Remaining tasks are done:`) stay
`end_turn = true`. Attributive complete (`Remaining complete tasks:`) and
hedged completion (`Remaining tasks are mostly done:`) still continue, as do
negated or incomplete remaining (`Remaining work is not done:`,
`Remaining work is incomplete:`).
Cleared remaining polarity
(`No issues remaining:`) stays `end_turn = true` unless a later unfinished
speaker cue is still present (`Nothing pending, but I still need to:`) or a
later clause still has speaker pending (`Nothing pending, verification is
pending:`). Bare
`pending` is a status label on some other actor or process
(`Approval pending:`, `Review pending:`, `CI pending:`) and stays
`end_turn = true`. Complement particles such as `back` and `up` are
stripped before the object is classified, so `I'll check back with you` and
`I'll follow up soon` stay `end_turn = true`. Wrap-up verbs, person
complements, leftover adverbs or state words, offer clauses on unlisted verbs
(`I'll take a look later if you want`, `I'll take another look later`),
generic pronouns (`I'll do it next`), and work verbs whose only complement is
postponement (`I'll continue later`, `I'll run soon`, `I'll wait later`)
also do not force a follow-up. Bare `I'll wait` stays a pause, but `Let me wait
for the agent` continues. Bare work verbs and immediacy still continue
(`I'll continue`, `I'll verify now`). Closings
such as `let me know`, `I'll leave the rest`, and delivery colons such as
`Here is the final report:` stay `end_turn = true`, but investigative
complements still continue (`Now let me know what failed in the test output`,
`Let me see the test output`, `Let me see if the tests pass`,
`I'll help fix the failing tests`). `if`/`whether`/`when` are person
hand-offs only when the clause addresses the user (`Let me see if you need
anything`, `Let me check if you need anything`); `Let me know if the tests
pass` stays a hand-off because `know` plus a conditional is an inform-me
request. Known work verbs may still
take a pronoun object (`I'll inspect it next`) and may keep a trailing
`if you want` after a real object (`I'll inspect the tree if you want`).
Person complements still win over work verbs (`look at your PR`). Unlisted
verbs still continue with a real object even when sequenced (`I'll update the
lockfile next`). A fully completed
`update_plan` suppresses the guard when it is still the latest intent (no
later tool work), but sessions that never call `update_plan` are still
covered, and a completed plan followed by real tool work does not hide a later
mid-task pause.
`max_followups` limits consecutive automatic continuations per
`prompt_cache_key`. The counter resets when the last request `input` item is
completed tool work (or a pending non-`update_plan` tool call), including
requests that are not themselves suspected pauses, so a long
session can keep auto-continuing through genuine mid-task pauses after real
tool progress without letting a text-only loop run forever. A trailing
`update_plan` does not count as progress. Tool outputs and chat `role=tool`
messages also do not count unless they match a non-`update_plan` call id
already present in the request. Requests without a
`prompt_cache_key` are observed but not forced.

Continue guard does not patch prompts, modify skills, or cross providers for
auto-review. It only changes the final Responses `end_turn` flag for a narrow
completion shape that Codex itself knows how to follow up.

CLI overrides are available for test sessions:

```bash
target/debug/codex-warp \
  --config configs/clinepass.toml \
  --continue-guard \
  --continue-guard-mode end_turn_false \
  --continue-guard-max-followups 1
```

The defaults already cover normal use; the CLI flags are for temporary tuning.

## Tool Approval Policy

Codex Warp can optionally apply a policy layer to selected downstream tool
calls before Codex executes them. The current implementation is conservative and
disabled by default:

```toml
[tool_policy]
enabled = true
mode = "assist"

[config]
tool_policy_include = ["configs/tool-policies/github.toml"]
```

Policy rule includes are additive. To replace the bundled policy rules instead
of extending them, set `tool_policy_replace = true` in the same config layer as
your replacement rules or replacement `tool_policy_include`.

The policy has four outcomes:

| Outcome | Meaning |
| --- | --- |
| `allow_hint` | Decorate simple recognized tool calls with `sandbox_permissions`, `prefix_rule`, and justification. |
| `manual` | Keep valid but complex commands reviewable without suggesting reusable approval. |
| `force_manual` | Require escalation without suggesting a reusable approval prefix. |
| `deny` | Block known unsafe commands such as token-printing operations. |

The design intentionally keeps Codex as the final execution authority. Warp
only makes approval requests cleaner and prevents credential-disclosure
commands from being emitted. See [Tool Approval Policy](tool-approval-policy.md)
for the TOML rule shape and GitHub policy table.

**Notice:** tool approval policy can change what Codex is told to approve,
prompt for, or block. Misconfigured rules are your responsibility. Review them
before enabling the feature and use it at your own risk.

## Web UI And Analytics

Codex Warp can serve a lightweight local Web UI for managing providers/models and
viewing usage analytics. It is disabled by default. Set `enabled = true` to
serve it on the same bind address as the proxy. Authentication is optional: by
default the UI has no authentication and requires a loopback listen address
(`127.0.0.1` or `[::1]`).
Set `auth_token_env` to protect `/api` with a bearer token read from that
environment variable; the browser asks for it only if the API returns 401.
When `auth_token_env` is configured, the named environment variable must be
present and non-empty at startup (fail closed). Omit the setting entirely for
unauthenticated loopback access.
A trusted-network deployment can opt in to remote access explicitly.

```toml
[webui]
enabled = true
auth_token_env = "CODEX_WARP_WEBUI_TOKEN" # Optional; omit for no authentication.
db_path = "codex-warp.db"
# Default false. Set true only on an access-controlled, trusted network.
allow_unauthenticated_remote_access = false
```

When `listen` is non-loopback, startup fails unless
`allow_unauthenticated_remote_access = true` is set. The existing setting remains
the explicit LAN-exposure gate whether authentication is configured or not.
Without `auth_token_env`, it is an intentionally unsafe compatibility switch
for trusted networks. Codex Warp does not terminate TLS, so authenticated remote
deployments should still use a trusted network or TLS reverse proxy.

After enabling it, open `http://<configured-listen-address>/ui/` while the
proxy is running (the startup log prints the exact URL). With the default
`listen` setting, this is `http://127.0.0.1:8787/ui/`.

The UI can:

- add providers from bundled example templates (OpenRouter, Hicap, Kimi Code,
  OpenCode Go, ClinePass, Xiaomi Token Plan, or a blank OpenAI-compatible
  profile); the template label is prefilled but editable, and adding the same
  named template again creates a uniquely identified instance so one gateway can
  be configured with multiple credentials
- edit and remove providers and model catalog entries
- toggle providers on/off (disabled providers are omitted from `/v1/models`)
- toggle models on/off per provider
- manually refresh an enabled provider's dynamic upstream model catalog
- chart token usage, prompts, and sessions over time with a line chart, plus
  token usage over time with a bar chart (global, per provider, and per model)
- chart model usage over time with per-model lines for sessions, prompts, and
  cache rate (cached tokens as a percent of input tokens; global, per provider,
  and per model)
- show window-scoped average cache rate beside the other analytics summary cards
- inspect token usage breakdowns with pie charts: provider usage (visible
  while All providers is selected and hidden when a provider is selected),
  model usage per provider (visible while a provider is selected and All
  models is selected; hidden otherwise), and overall model usage (always
  visible; global even while a provider filter is active)
- view process logs and the sanitized debug JSONL log, and change debug logging
  settings without restarting Warp

The Analytics tab is the default landing view.

Ranges: `1h`, `5h`, `today` (UTC midnight boundary), `24h`, `48h`, `3d`,
`week`, `30d`, `yearly`.

SQLite (`db_path`) stores overlays and usage analytics. TOML remains the
bootstrap source of truth; overlays apply on startup whenever the database is
open. Managed providers created in the UI live entirely in SQLite. Debug logging
settings saved in the Logs tab are stored in a single-row debug overlay and
replayed with the provider/model overlays. Removing a
TOML-sourced provider or catalog model soft-deletes it via an overlay so it
stays suppressed across restarts until the overlay row is cleared or the model
is re-added in the UI. Soft-deleting a TOML provider keeps its per-model
overlay rows so clearing the provider soft-delete can restore prior model
toggles. Creating a managed provider with the same id (for example adding a
bundled template after deleting the TOML profile) replaces those leftover
model overlays with the new catalog. Soft-deleting a catalog model also
suppresses its upstream alias so live `/models` fetches cannot resurrect it.
Enabled model overlays reseed `model_routes` at startup so multi-provider
routing does not require a prior `/v1/models` call after restart.

The SQLite store opens only while the Web UI is enabled. Use
`--no-webui-store` to keep an enabled UI stateless; its management API then
keeps read-only provider/template views available, while provider/model
mutations and usage analytics return service-unavailable rather than writing
persistent state. Logging settings still apply live without SQLite; they are
not kept across restart until the store is open.

`PUT /api/providers/{id}/models/{model_id}` is a partial update: omitted fields
keep their current values, and JSON `null` clears optional string fields
(`upstream_id`, `display_name`, `description`). Omitting `enabled` does not
re-enable a disabled model. `POST /api/providers/{id}/models` still creates or
replaces a full catalog entry (with `enabled` defaulting to true when omitted).
`POST /api/providers/{id}/refresh-models` re-fetches one enabled provider's
upstream `/models` catalog and atomically updates its live model view and route
map. A successful response adds newly reported models and removes live-only
models no longer returned by that provider; configured catalog entries and
persisted operator overlays remain authoritative. Static
(`model_catalog_only = true`) and disabled providers cannot be refreshed. A
failed fetch preserves the last discovered routes. The action requires a JSON
body and is unavailable with `--no-webui-store`.

The model editor also shows the effective reasoning modes for automatically
discovered models. Editing one promotes its exact live slug to a catalog
override. `supported_reasoning_levels` and `default_reasoning_level` can be set
while adding or editing a model; omitting them preserves inheritance, while
JSON `null` clears an existing override. Discovery metadata is retained per
provider, so providers that report the same slug keep their own modes. A
temporarily disabled model, and a catalog alias whose upstream slug is no
longer in the latest fetch, keep the metadata from the last successful
catalog refresh instead of falling back to synthetic `none` modes.

`PUT /api/providers/{id}` is a partial update: omitted fields keep their current
values, and JSON `null` clears `api_key`, `api_key_env`, `name`, and `headers`
(an empty headers object also clears). Header maps are HTTP-case-insensitive:
two names that differ only by ASCII case are rejected, and names or values that
are not valid HTTP headers are rejected so they cannot be stored and then
silently dropped on the upstream request. Managed (Web UI-created) providers
persist `api_key`, `api_key_env`, and request `headers` in the SQLite overlay
because they have no TOML snapshot. `GET /api/providers` never returns a raw
inline key. Managed views report `has_inline_api_key`, an `api_key_preview` that
shows only a short prefix and suffix, and `has_api_key` when a usable key can be
resolved (inline or from `api_key_env`). TOML-backed views omit `api_key_preview`
and header values because TOML remains the source of truth. Overlay headers are
returned for managed providers so the editor can round-trip them. TOML-backed
overlays still strip `api_key` and headers, and ignore Web UI `headers` and
credential patches. For a TOML-backed provider, `api_key` and `api_key_env`
remain TOML-owned: the Web UI cannot set or clear them, so a later TOML
credential rotation cannot be overwritten by an old SQLite snapshot. In the Web
UI editor, a value is classified as `api_key_env` when it matches ASCII uppercase,
digits, and underscores, starts with `A-Z` or `_`, and contains at least one
underscore (for example `OPENROUTER_API_KEY`). Every other value is stored as an
inline `api_key`. Environment variable names stay visible in `GET /api/providers`;
inline keys do not. Clearing an environment variable name or using Clear saved
credentials sends JSON `null` for both credential fields. A `PUT` that sends
`null` for only one credential field also clears the other, because a provider
has a single credential slot. A masked saved key is
not editable in place: leaving it unchanged omits the fields and keeps the stored
secret; replacing it requires Clear saved credentials, then a new value.
Leaving a loaded environment variable name unchanged omits the credential fields.
Editing a stored environment variable name into a truncation of that name
(for example `OPENAI_API_KEY` → `OPENAI`) is rejected by the editor and by
`PUT /api/providers/{id}`. Creating a provider from a named example template
rejects the same truncation against the template's bundled env name on
`POST /api/providers`. Unrelated values such as `AKIA…` are treated as a new
inline key. Pasting a masked preview (a value shaped like `mask_api_key`, with
a run of `•` characters) is rejected by
the editor and by `PUT`/`POST` `/api/providers`. Managed overlay databases are
opened with owner-only permissions (`0600` on Unix) because they may contain
inline `api_key` values. If a managed overlay row disappears while Codex Warp is
still running, this process still treats that provider as managed so a later
save or enable toggle cannot rewrite it as TOML-backed and drop the inline key.

CLI overrides:

```bash
codex-warp --no-webui          # skip /ui and /api routes and leave SQLite unopened
codex-warp --webui-db /var/lib/codex-warp/codex-warp.db
```

`--no-webui` and `webui.enabled = false` disable all Web UI management
features, including SQLite overlays and usage recording.

Usage events are recorded from successful proxied responses when the store is
open, including completed chat and native streams and successful non-stream
responses even when the upstream omits token usage metadata. Well-formed native
Responses terminals with `status: "incomplete"` (including
`response.incomplete` stream events) are also recorded when they carry usage,
because they consumed tokens before stopping. Failed, malformed, and
provider-error-envelope streams are not recorded as successful completions.
Session grouping prefers `prompt_cache_key`, then `conversation_id`, then Responses
`conversation` (string or `{ "id": ... }`).
Events without a session key count as distinct sessions per prompt.

## Debug Logging

Codex Warp can write sanitized JSONL debug events for local troubleshooting:

```toml
[debug]
enabled = true
log_path = "/tmp/codex-warp-debug.jsonl"
include_bodies = false
include_stream_bodies = false
max_log_mb = 128
max_log_age_days = 30
# Optional. When unset, Warp uses RUST_LOG or `info` captured at tracing start.
# tracing_filter = "codex_warp=debug"
```

You can also enable it from the command line:

```bash
target/debug/codex-warp \
  --config configs/moonshot-kimicode.toml \
  --debug-log /tmp/codex-warp-debug.jsonl
```

The Web UI Logs tab can tail process logs (tracing) and the debug JSONL file,
and it can change these `[debug]` settings without restarting Warp. Saves persist
in the SQLite debug overlay and replay on the next start. `--debug-log` and the
body-inclusion flags still win over that overlay for the current process.

`GET /api/logging` returns the live settings, including `persist_available`.
`persisted` is only meaningful on `PUT /api/logging`: it is true when that
mutation wrote the SQLite overlay. GET always returns `persisted: false`.
`max_log_mb` and `max_log_age_days` are the stored snapshot values (`null` when
unset). `max_log_mb_effective` and `max_log_age_days_effective` are the limits
the writer uses (`128` / `30` when those fields are unset). The Logs form
hydrates empty rotation and log-path fields from the stored values and shows the
effective / default destinations as placeholders, so saving other settings does
not persist explicit defaults. Switching away from the Logs tab does not rewrite
those fields while the form has unsaved edits. Live GET still refreshes the persist
hint and placeholders (not the field values) so tracing lag and effective defaults
stay current. A successful save applies the PUT response into the form unless the
user edited fields while that request was in flight; the footer then reports that
the submitted snapshot was applied and unsaved edits remain. A failed save keeps
the unsaved edits. `PUT /api/logging` validates the full live snapshot first, including the
tracing filter that will actually be reloaded (`tracing_filter`, or the process
default captured from `RUST_LOG` / `info` when tracing started). Live logging
has one snapshot, stored by `DebugLog`.
`GET /api/logging`, debug events, and request logging all read that snapshot.
Boot `[debug]` (TOML, overlays, CLI) is applied into that snapshot at startup
and is not kept as a second live copy in `AppConfig`. After the snapshot is
committed, Warp reloads tracing as a best-effort projection. `tracing_filter`
is the requested live setting; `tracing_filter_wanted` is that setting resolved
to the filter Warp will reload (`tracing_filter`, or the process default
captured from `RUST_LOG` / `info` when tracing started — later `RUST_LOG`
changes are not re-read); `tracing_filter_effective` is the filter the
subscriber last installed successfully; `tracing_applied` is true when a
subscriber is installed and wanted and effective match. If no tracing
subscriber is installed, `tracing_filter_effective` is empty,
`tracing_filter_wanted` resolves unset filters to `info` (without re-reading
`RUST_LOG`), and `tracing_applied` is false. Debug overlays are validated the
same way: unset `tracing_filter` is checked against the pinned process default
from tracing init (or `info` when replaying without that pin), never against a
live `RUST_LOG` read. If tracing reload fails, the live snapshot stays
applied, `tracing_applied` is false, and process logs keep the previous
verbosity until a later save retries the filter. The SQLite overlay is durability, not live state: it is written
after live install and never stores a snapshot that failed to become live.
Overlay writes parse `tracing_filter` the same way as live apply and replay
(using `info` when the process pin is unavailable), so an invalid filter cannot
be stored. Overlay persist failure does not reinstall live settings and does not fail the
request; PUT returns the applied live settings with `persisted: false`. The next
start still replays the previous overlay until a later save persists. A crash
after live apply and before the overlay write does not persist across restart;
the next start replays the previous overlay. A crash after the overlay write
still replays the new overlay on the next start.
`GET /api/logging/events` reads recent process events or the current JSONL tail
(`source=process|debug`). A debug tail pins the writer snapshot (`enabled`,
path, and file descriptor) under the writer lock, then parses after releasing
that lock so request logging is not blocked. `enabled` is the writer flag from
that pin, not a later config read and not “file exists”. If the file rotates
during that parse, the response may show the previous segment until the next
poll; events are still in the rotated backup. Debug `log_path`
values are validated for TOML, CLI, overlays, and the Web UI when debug logging
is enabled: the path must end in `.jsonl`, must not contain `..`, the log file
itself must not be a symlink, its parent directory must already exist, and the
resolved destination cannot use system roots such as `/etc`. Relative
paths are resolved against the process working directory at apply time, and the
live snapshot stores that destination so later writes and tails do not depend
on a later cwd change. SQLite overlay writes use the same pin, so a relative
`log_path` is not stored. Overlay replay still rewrites a relative path left by
an older Warp so the next restart does not depend on cwd either. A relative `log_path` is rejected when Warp's cwd is a
restricted root. A path stored while
logging is disabled is not opened and does not fail startup. Warp does not
follow a symlink at the log file itself when writing or tailing
(`O_NOFOLLOW` on Unix, `FILE_FLAG_OPEN_REPARSE_POINT` on Windows). Parent
directories are resolved to their real location so a symlink parent cannot place
the log under a restricted root. Enabling debug logging opens (and creates) the
log file immediately; a missing parent or unwritable path fails that apply
instead of reporting enabled while writes silently drop. When `debug.enabled` is true and
`log_path` is omitted, Warp uses `codex-warp-debug.jsonl` in the process working
directory. `max_log_mb` and `max_log_age_days` of `0` are invalid at every entry
point: the Web UI rejects them, startup fails, and overlays that contain them
are skipped.

### Rotation

When `log_path` points to an existing file, Warp rotates it before startup and
before each write once it reaches `max_log_mb` megabytes (default `128`) or the
current log file is `max_log_age_days` days old (default `30`), whichever comes
first. Age is measured from the log file's creation time when the platform
provides it (the start of the current log segment after the last rotation),
otherwise from its last modification time. On filesystems without birth time,
each append refreshes the modification time, so age-based rotation only
applies while the log is idle; actively written logs on those hosts rely on
the size limit until they stop receiving events. The current log is renamed to
`{log_path}.1` and a fresh log file is started. Only one backup is kept, so a
second rotation overwrites the previous backup.

Set `max_log_mb` and `max_log_age_days` in TOML:

```toml
[debug]
enabled = true
log_path = "/tmp/codex-warp-debug.jsonl"
max_log_mb = 64
max_log_age_days = 7
```

Values of `0` are invalid. Rotation is
performed by the individual Warp process; if multiple Warp instances share the
same `log_path`, concurrent rotations can race and the backup may be overwritten
or removed unexpectedly. Use a distinct `log_path` per instance when running
more than one Warp process.

Each request writes an `upstream_request` event with the selected provider,
backend, model, `prompt_cache_key`, `stream_options`, metadata presence flags,
message/input/tool fingerprints, and a full transformed-body fingerprint.
Responses write `upstream_response` events with the raw provider `usage` object
and, for chat-completions providers, Codex Warp's normalized usage.

For chat-completions transforms, `upstream_request.transform` also records
redacted transform decisions:

- `dropped_request_fields`: top-level request fields that were not sent
  upstream, such as Codex-local metadata.
- `added_request_fields`: top-level fields created by the transform, such as
  `messages`.
- `original_tool_count` and `converted_tool_count`: the before/after tool
  counts.
- `tool_transforms`: tool name, tool type, action, and reason. Tool arguments
  and schemas are not copied into this summary.
- `messages_with_reasoning_content` and `messages_with_tool_calls`: follow-up
  request counts useful for preserved-thinking and tool-call history checks.

For streamed responses, Codex Warp writes redacted `upstream_stream_delta`
events when a chunk carries observable content, reasoning, or tool-call signal.
These events include field names and character counts such as
`reasoning_content_chars`, `reasoning_chars`,
`emitted_reasoning_delta_events`, and `tool_call_delta_count`; they do not
include the actual text.

By default the log does not include prompt text or response text. For short
local troubleshooting sessions where you need the full transformed request,
non-stream upstream response bodies, raw upstream SSE frames, and exact
Warp-to-Codex downstream SSE frames, enable:

```toml
[debug]
enabled = true
log_path = "/tmp/codex-warp-debug.jsonl"
include_bodies = true
include_stream_bodies = true
max_log_mb = 128
max_log_age_days = 30
```

or pass:

```bash
target/debug/codex-warp \
  --config configs/moonshot-kimicode.toml \
  --debug-log /tmp/codex-warp-debug.jsonl \
  --debug-log-include-bodies \
  --debug-log-include-stream-bodies
```

When `include_stream_bodies` is enabled, the debug log records
`upstream_stream_frame` and `downstream_stream_frame` events. Use these to
compare the provider's raw streamed reasoning fields against the exact
Responses SSE events Codex receives.

Even with full-body logging enabled, Codex Warp redacts structured secret fields
and obvious provider tokens before writing JSONL. Do not share a full-body debug
log unless you have still reviewed it for prompt, response, streamed reasoning,
and tool-result content.
