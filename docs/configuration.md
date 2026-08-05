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
- [Debug Logging](#debug-logging)

## Baseline Includes

The baseline config does not enable an upstream provider by default, but it does
load model-family catalogs and bundled tool-policy rules:

```toml
[config]
# Optional provider profiles:
# include = [
#   "configs/clinepass.toml",
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

Codex Warp also attaches [OpenRouter app attribution](../README.md#openrouter-app-attribution)
headers on every upstream request (`HTTP-Referer`, `X-OpenRouter-Title`, `X-Title`,
and `X-OpenRouter-Categories`). Set any of those names under `[provider.headers]`
or `[providers.<id>.headers]` to override the automatic values for that gateway.

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
- `transform.request_stream_options_include_usage`: when `true`, streamed
  chat-completions requests that do not already include `stream_options` get
  `stream_options.include_usage = true`. Use this only for gateways that
  document support for that field.

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
mode = "observe"
max_followups = 1
```

Modes:

- `observe`: log `continue_guard` debug events for suspected premature stops,
  but leave `end_turn = true`. Use this to confirm a provider is hitting the
  pattern before turning on automatic continuation.
- `end_turn_false`: for suspected premature stops, emit
  `response.completed.response.end_turn = false` so Codex continues the turn.
  Use this for providers that have been verified to stop mid-plan.

The guard only applies to chat-completions streams that finish with
`finish_reason = "stop"`, emit no tool call, have an active `update_plan` in the
request history, and end with continuation phrasing. `max_followups` limits
automatic continuations per `prompt_cache_key`; requests without a
`prompt_cache_key` are observed but not forced.

Continue guard does not patch prompts, modify skills, or cross providers for
auto-review. It only changes the final Responses `end_turn` flag for a narrow
streaming completion shape that Codex itself knows how to follow up.

CLI overrides are available for test sessions:

```bash
target/debug/codex-warp \
  --config configs/clinepass.toml \
  --continue-guard \
  --continue-guard-mode end_turn_false \
  --continue-guard-max-followups 1
```

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

## Debug Logging

Codex Warp can write sanitized JSONL debug events for local troubleshooting:

```toml
[debug]
enabled = true
log_path = "/tmp/codex-warp-debug.jsonl"
include_bodies = false
include_stream_bodies = false
```

You can also enable it from the command line:

```bash
target/debug/codex-warp \
  --config configs/moonshot-kimicode.toml \
  --debug-log /tmp/codex-warp-debug.jsonl
```

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
