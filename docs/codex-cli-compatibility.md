# Codex Client Compatibility Notes

Codex Warp serves the `/v1/models` metadata that Codex Desktop, Codex CLI, and
other Codex clients use to decide how a model should be driven. Slash-command
UI behavior is local to the Codex client, but some commands depend on model
metadata and request compatibility once they start a model turn.

## Table Of Contents

- [Checked Source Paths](#checked-source-paths)
- [What Codex Warp Must Preserve](#what-codex-warp-must-preserve)
- [Structured Output And Chat Stream Compatibility](#structured-output-and-chat-stream-compatibility)
- [Guardian Auto-Review Compatibility](#guardian-auto-review-compatibility)
- [Sub-Agent Namespace Helpers](#sub-agent-namespace-helpers)
- [Codex App Server Model Refresh](#codex-app-server-model-refresh)
- [Configurable Codex Model Metadata](#configurable-codex-model-metadata)

## Checked Source Paths

These notes were checked on 2026-07-02 against the public
[`openai/codex`](https://github.com/openai/codex) source tree at commit
[`020828170fb2224f0d7a7a243a1f7d21cc3df5ee`](https://github.com/openai/codex/commit/020828170fb2224f0d7a7a243a1f7d21cc3df5ee):

- [`codex-rs/tui/src/slash_command.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/tui/src/slash_command.rs)
  lists built-in slash commands such as `/model`, `/skills`, `/plugins`,
  `/goal`, and `/compact`.
- [`codex-rs/protocol/src/openai_models.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/protocol/src/openai_models.rs)
  defines the `ModelInfo` fields Codex expects from `/models`.
- [`codex-rs/core/src/context/available_skills_instructions.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/core/src/context/available_skills_instructions.rs)
  shows that `include_skills_usage_instructions` controls whether skill usage
  instructions are included in model context.
- [`codex-rs/core/src/compact_remote.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/core/src/compact_remote.rs)
  shows that `/compact` sends a compaction request through the normal model
  client.
- [`codex-rs/features/src/lib.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/features/src/lib.rs)
  shows that plugins, goals, and remote compaction are Codex feature flags
  rather than provider-advertised slash commands.
- [`codex-rs/models-manager/src/manager.rs`](https://github.com/openai/codex/blob/020828170fb2224f0d7a7a243a1f7d21cc3df5ee/codex-rs/models-manager/src/manager.rs)
  seeds custom providers from bundled models and merges refreshed `/models`
  entries into that catalog for API/command-auth providers.

## What Codex Warp Must Preserve

`/skills` and user-created skills are discovered by Codex locally, but the model
still needs the skills context. Synthetic model entries default
`include_skills_usage_instructions = true`, and provider/model-family catalogs
can override it when needed.

`/plugins` is also local Codex functionality. Plugins may contribute skills, MCP
servers, app connectors, and hooks. Codex Warp does not install or list plugins,
but it must avoid breaking the model turn that follows plugin or skill
injection.

`/goal` is persisted and managed by Codex. It does not require special provider
routes, but goal continuations still use the selected model through Codex Warp.

`/compact` uses the model client, so the provider must handle the translated
Responses request. Context and compaction metadata are configurable through
`context_window`, `max_context_window`, `auto_compact_token_limit`,
`effective_context_window_percent`, and `comp_hash`.

`/model` uses the normalized model catalog. Codex Warp merges provider `/models`
results with provider and model-family metadata so models from multiple
providers can appear in one catalog.

## Structured Output And Chat Stream Compatibility

Recent Codex CLI versions send a separate guardian request when deciding whether
an agent action may self-approve. That request uses Responses `text.format` with
`type = "json_schema"`. Warp still converts that field to Chat Completions
`response_format.type = "json_schema"` first, so gateways that support strict
structured output keep it.

If the upstream returns HTTP 400 and the error object is clearly about an
unsupported `response_format` type, JSON Schema, or unavailable structured
output, Warp retries the same request once with
`response_format.type = "json_object"` and a concise system instruction to
return one JSON object matching the original schema. The retry is global Chat
Completions behavior, not a per-provider workaround and not a tool-policy
decision. Unrelated 400s, authentication errors, rate limits, and timeouts are
not retried. If `json_object` is also rejected as an unsupported response
format, Warp returns a structured-output incompatibility error so Codex can
require manual approval. Other fallback failures are forwarded and do not mark
the model as incompatible.

A short-lived in-memory cache keyed by upstream base URL plus model remembers
whether that pair supports `json_schema`, only `json_object`, or no structured
output, then expires so later requests can probe again.

Some OpenAI-compatible chat streams omit the terminal `[DONE]` marker even after
they emit a semantic `finish_reason` such as `stop` or `tool_calls`. Warp still
requires `[DONE]` when that terminal reason is missing, but it synthesizes the
normal Responses completion sequence when the stream ends cleanly after a
documented terminal `finish_reason`. Truncated streams and mid-stream transport
errors still fail.

Some Hy3 gateways stream multiple invocations of the same function as adjacent
JSON objects under one tool-call index. The Hy3 family enables a narrow output
repair that separates that invalid argument string into distinct Responses tool
calls. A single JSON object, incomplete JSON, non-object JSON, or trailing junk
is forwarded unchanged rather than guessed at. Repair runs only after a
successful tool-call finish and shares a response-wide limit of 64 recovered
calls and 1 MiB of source argument text inspected for recovery across streaming
and JSON responses.

## Guardian Auto-Review Compatibility

Codex sends a separate Guardian request when reviewing whether an agent action
should be approved. Those requests use a `prompt_cache_key` that starts with
`guardian:`.

Codex normally replaces the review sentinel with the active model's
`auto_review_model_override` from `/v1/models`. If a client sends the literal
`codex-auto-review` instead, Warp resolves it to the concrete model it observed
for the matching session (`guardian:<prompt_cache_key>`). Configure
`[config].auto_review_model` to override that session-aware default. Warp
leaves the request unresolved when neither source is available rather than
guessing between multiple configured gateways.

Warp does not decide allow or deny locally. It still forwards Codex's Guardian
policy, transcript, planned action, schema, tools, and model. For Guardian
Chat Completions requests only, it appends a short system clarification that
the Guardian's own read-only and no-network restrictions do not forbid the
coding agent from requesting escalation. Ordinary coding turns, tool
continuations, and non-Guardian Responses requests do not receive that
clarification.

If the Guardian request also needs structured output, the JSON Schema fallback
still applies independently. The prompt shim is about decision semantics; the
fallback is about making the JSON response parseable.

## Sub-Agent Namespace Helpers

These notes were checked on 2026-08-26 against the public
[`openai/codex`](https://github.com/openai/codex) source tree at commit
[`bde9db1375667c50dcc0c2b52532a4e2672571c2`](https://github.com/openai/codex/commit/bde9db1375667c50dcc0c2b52532a4e2672571c2):

- [`codex-rs/core/src/tools/handlers/multi_agents_spec.rs`](https://github.com/openai/codex/blob/bde9db1375667c50dcc0c2b52532a4e2672571c2/codex-rs/core/src/tools/handlers/multi_agents_spec.rs)
  wraps v1 `spawn_agent`, `send_input`, `resume_agent`, `wait_agent`, and
  `close_agent` in a Responses `type = "namespace"` tool named
  `multi_agent_v1`.
- [`codex-rs/core/src/tools/spec_plan.rs`](https://github.com/openai/codex/blob/bde9db1375667c50dcc0c2b52532a4e2672571c2/codex-rs/core/src/tools/spec_plan.rs)
  places v2 `spawn_agent`, `send_message`, `followup_task`, `wait_agent`,
  `interrupt_agent`, and `list_agents` in the `collaboration` namespace by
  default.
- [`codex-rs/core/src/tools/router.rs`](https://github.com/openai/codex/blob/bde9db1375667c50dcc0c2b52532a4e2672571c2/codex-rs/core/src/tools/router.rs)
  routes a Responses function call from separate `namespace` and `name`
  fields. A dotted name without a namespace is an ordinary function name and
  does not select the collaboration runtime.
- [`codex-rs/core/src/session/multi_agents.rs`](https://github.com/openai/codex/blob/bde9db1375667c50dcc0c2b52532a4e2672571c2/codex-rs/core/src/session/multi_agents.rs)
  tells the root model that agent starts are asynchronous, identifies the
  available concurrency slots, and distinguishes queued `send_message` from
  turn-triggering `followup_task`.

Chat Completions providers and many OpenAI-compatible Responses backends do
not understand namespace tools. Warp expands each namespace child into an
ordinary function named after the child (`spawn_agent`, `wait_agent`, and so
on) and keeps the original child description and parameter schema. Responses
`encrypted` schema annotations are remembered but removed from the ordinary
function schema sent to third-party providers because they are not standard
JSON Schema keywords. If a child name is already present as a top-level
function, Warp uses a provider-safe generated alias such as
`collaboration__spawn_agent` (with a numeric suffix if necessary).

On the way back to Codex, Warp restores the child name and namespace as
separate fields. Message-bearing v2 calls also receive an empty
`encrypted_function_args` marker, which tells Codex that the compatible
third-party backend returned plaintext arguments. Warp also unwraps the older
collapsed envelope
`{ "tool": "spawn_agent", "arguments": { ... } }` that some models emit when
they only saw a single `{namespace}_tool` function.

Plaintext v2 messaging currently requires Codex's default `collaboration`
namespace. The pinned Codex protocol recognizes the empty plaintext marker
only for that runtime namespace; a custom `multi_agent_v2.tool_namespace`
would otherwise label plaintext tasks and updates as encrypted content. Warp
rejects such requests with a compatibility error instead of silently dropping
the child task or mailbox payload. Full custom-namespace support requires a
corresponding Codex router change.

For Chat Completions and compatible native Responses coding turns (not
Guardian requests), Warp also inserts a short clarification that sub-agent
tools are ordinary functions and lists the exact aliases advertised in that
request. The clarification recommends starting multiple independent agents
before waiting when Codex reports available slots and identifies the
advertised messaging lifecycle. On Chat backends, Warp also translates Codex
v2 `agent_message` task, update, and result items into user-role messages with
their author and recipient context; encrypted content is explicitly marked as
unavailable rather than silently discarded. Codex remains responsible for the
actual per-session concurrency limit, agent execution, message delivery, and
result notifications; Warp only preserves their request and response protocol.

Empty namespaces still collapse to `{namespace}_tool` as a last-resort helper.

## Codex App Server Model Refresh

When using Codex Warp as a Codex provider, do not set `model_catalog_json` in
Codex's `config.toml`. That option makes Codex build a static model manager from
the JSON file and prevents the app server from auto-populating models from the
provider's `/v1/models` endpoint.

For Codex Warp, Codex should instead be configured with a normal provider entry
whose `base_url` points at the local proxy, for example
`http://127.0.0.1:8787/v1`.

Current Codex CLI only refreshes a custom provider's remote model catalog when
the provider uses Codex backend auth or has command-backed provider auth
configured. Because Codex Warp owns the upstream credentials in its gateway
configs, use a harmless local auth command on the Codex provider entry:

```toml
[model_providers.codex-warp.auth]
command = "printf"
args = ["codex-warp-local"]
refresh_interval_ms = 0
```

After removing `model_catalog_json` and adding the auth shim, restart the Codex
app-server daemon so it rebuilds its model manager and fetches the merged Codex
Warp catalog from the live proxy. The shim is only a local catalog-refresh
trigger; upstream provider API keys still belong in Codex Warp gateway configs.

Codex's command-auth refresh path merges remote models into the bundled Codex
catalog rather than replacing it. The bundled GPT models use low priorities, so
they can appear interleaved with gateway models unless Codex Warp overrides
them. `hide_codex_builtin_models = true` is enabled by default under Warp's
`[config]` section; it appends hidden replacements for Codex's bundled GPT
slugs so the picker only shows the live gateway catalog. Set it to `false` only
when intentionally testing Codex's bundled models alongside Warp gateways.

## Configurable Codex Model Metadata

Catalogs can set the Codex-facing fields below without recompiling:

- `include_skills_usage_instructions`
- `experimental_supported_tools`
- `tool_mode`
- `multi_agent_version`
- `auto_review_model_override`
- `comp_hash`
- `effective_context_window_percent`
- `auto_compact_token_limit`
- `context_window` and `max_context_window`
- tool/search fields such as `shell_type`, `apply_patch_tool_type`, and
  `web_search_tool_type`

When a provider returns these fields directly from `/models`, Codex Warp now
passes them through before applying model-family and provider overrides.
