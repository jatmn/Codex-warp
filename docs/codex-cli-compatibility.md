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

## Guardian Auto-Review Compatibility

Codex sends a separate Guardian request when reviewing whether an agent action
should be approved. Those requests use a `prompt_cache_key` that starts with
`guardian:`.

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
