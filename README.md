# Codex Warp

Codex Warp is a tiny Rust proxy that lets Codex Desktop, Codex CLI, and other
Codex clients talk to OpenAI-compatible providers through a local Responses API
surface.

Codex sends Responses-shaped requests with tools, streaming, metadata, and
newer tool types such as `custom`. Many third-party providers only expose
`/v1/chat/completions`, or they expose partial Responses support. Codex Warp
translates those requests with editable TOML config so provider quirks can be
fixed without recompiling.

## Table Of Contents

- [What It Does](#what-it-does)
- [Quick Start](#quick-start)
- [Key Features](#key-features)
- [Built-In Gateway Profiles](#built-in-gateway-profiles)
- [Supported Model Families](#supported-model-families)
- [More Docs](#more-docs)
- [Current Scope](#current-scope)
- [Affiliation](#affiliation)
- [License](#license)

## What It Does

Codex Warp sits between Codex and an upstream OpenAI-compatible provider. Codex
continues to use the Responses API shape it expects, while Warp adapts the
request, model catalog, stream events, and provider-specific fields on the way
through.

It is meant for provider compatibility work that should live in config instead
of client patches:

- local `/v1/responses` and `/v1/models` endpoints for Codex
- one or many upstream OpenAI-compatible gateways
- merged upstream and local model catalogs for Codex model selection
- model-family metadata for reasoning, tools, context windows, modalities, and
  provider-local auto-review routing
- editable request/tool morphs for chat-completions and Responses backends
- optional continue guard for chat-completions models that stop mid-plan
- optional tool approval policy loaded from TOML
- optional local Web UI for provider/model toggles and SQLite usage analytics
- sanitized debug logging and upstream `User-Agent` reporting

## Quick Start

Build the proxy:

```bash
cargo build
```

Start it with a provider profile:

```bash
export XIAOMI_TOKEN_PLAN_API_KEY="..."
target/debug/codex-warp --config configs/xiaomi-token-plan.toml
```

For Moonshot KimiCode:

```bash
export KIMICODE_API_KEY="..."
target/debug/codex-warp --config configs/moonshot-kimicode.toml
```

For OpenCode Go:

```bash
export OPENCODE_GO_API_KEY="..."
target/debug/codex-warp --config configs/opencode-go.toml
```

Point Codex at the local proxy:

```toml
model_provider = "codex-warp"

[model_providers.codex-warp]
name = "Codex Warp"
base_url = "http://127.0.0.1:8787/v1"
wire_api = "responses"
requires_openai_auth = false

[model_providers.codex-warp.auth]
command = "printf"
args = ["codex-warp-local"]
refresh_interval_ms = 0
```

Codex Warp owns the upstream provider credentials in its gateway config. The
Codex-side auth block above is only a local refresh shim so Codex
auto-populates models from the proxy.

Confirm the proxy can load models:

```bash
curl -sS http://127.0.0.1:8787/v1/models
```

## Key Features

**Config-Driven Provider Compatibility**

Provider profiles, model-family metadata, request transforms, tool transforms,
and tool approval rules are plain TOML under [`configs/`](configs/). The
baseline [`codex-warp.toml`](codex-warp.toml) loads model-family metadata and
tool policy rules, but it does not connect to any upstream provider by default.

**Merged Model Catalogs**

Warp merges upstream `/models` responses with local catalog entries, then adds
the Codex metadata the client needs for model selection. Provider profiles can
override exact model metadata when a gateway reports something unusual.

**Continue Guard**

Some chat-completions providers finish with text like `Now let me check...`
instead of issuing the next tool call. The continue guard can detect that case
while Codex has an active plan and ask Codex to continue the same turn with
`end_turn = false`:

```bash
target/debug/codex-warp \
  --config configs/clinepass.toml \
  --continue-guard \
  --continue-guard-mode end_turn_false
```

The guard is conservative: it only acts when Codex has an active plan, the
provider finishes with `finish_reason = "stop"`, no tool call was emitted, and
the assistant text looks like it intended to keep working. See the
[configuration guide](docs/configuration.md#continue-guard) for observe mode and
follow-up limits.

**Tool Approval Policy**

Warp can load tool approval rules from TOML and apply them before tool calls
reach Codex. Use this to add approval hints for known-safe commands, force a
manual prompt for interactive or sensitive commands, or deny requests that
should never be sent to the client.

**Notice:** tool approval policy can affect what Codex is told to approve,
prompt for, or block. Review every rule before enabling it. You are responsible
for your own policy configuration and use it at your own risk.

See the [tool approval policy guide](docs/tool-approval-policy.md) for the rule
format and the [configuration guide](docs/configuration.md#tool-approval-policy)
for deployment examples.

**Local Web UI**

Warp can serve a lightweight management UI at `/ui/` on the same listen address.
It is disabled by default; set `[webui] enabled = true` to turn it on. Use it to
add or edit providers and models, toggle them with switches, and chart
token/session usage from a local SQLite database. See the
[configuration guide](docs/configuration.md#web-ui-and-analytics).

## Built-In Gateway Profiles

| Profile | File | Purpose | Enabled by default |
| --- | --- | --- | --- |
| OpenAI-compatible template | [`configs/openai-compatible.toml`](configs/openai-compatible.toml) | Copy or edit for any provider with OpenAI-style auth and endpoints. | No |
| ClinePass | [`configs/clinepass.toml`](configs/clinepass.toml) | Ready profile for ClinePass with a local documented model catalog. | No |
| Moonshot KimiCode | [`configs/moonshot-kimicode.toml`](configs/moonshot-kimicode.toml) | Ready profile for Moonshot KimiCode subscription keys with a local Kimi model catalog fallback. | No |
| OpenCode Go | [`configs/opencode-go.toml`](configs/opencode-go.toml) | Ready profile for OpenCode Go subscription keys, limited to its OpenAI-compatible chat-completions models. | No |
| Xiaomi Token Plan | [`configs/xiaomi-token-plan.toml`](configs/xiaomi-token-plan.toml) | Ready profile for `https://token-plan-sgp.xiaomimimo.com/v1`. | No |
| OpenRouter | [`configs/openrouter.toml`](configs/openrouter.toml) | Ready profile for OpenRouter; app attribution headers are attached on all upstream requests. | No |
| Destination override | `--destination https://provider.example/v1` | Quick one-off target without editing provider config. | Only when passed |

## OpenRouter App Attribution

Codex Warp automatically attaches [OpenRouter app attribution](https://openrouter.ai/docs/app-attribution)
headers on **every upstream request** — for all configured gateways, models, and
API paths (`/chat/completions`, native `/responses`, `/models`, and any other
outbound call) — not only when the [`configs/openrouter.toml`](configs/openrouter.toml)
profile is the default gateway. OpenRouter documents attribution across all of
its API routes and models; Warp always sends the headers so no gateway/model
combination can skip them.

- `HTTP-Referer`: `https://github.com/jatmn/Codex-warp`
- `X-OpenRouter-Title`: `Codex Warp`
- `X-Title`: `Codex Warp` (backwards-compatible alias)
- `X-OpenRouter-Categories`: `cli-agent,programming-app`

These are Codex Warp's own identity values. To override any of them for a
specific provider, set the header under that provider's `[provider.headers]` or
`[providers.<id>.headers]` section — user-supplied headers always take
precedence over the automatic ones.

Note: `HTTP-Referer` is Codex Warp's public GitHub URL, so traffic sent through
OpenRouter is attributed under that identity in OpenRouter's public rankings.
To attribute traffic to your own project instead, override `HTTP-Referer` (and
the other headers) under `[provider.headers]` or `[providers.<id>.headers]`.

## Supported Model Families

| Parent brand | Catalog | Examples covered |
| --- | --- | --- |
| DeepSeek | [`configs/model-families/deepseek.toml`](configs/model-families/deepseek.toml) | `deepseek-v3.2`, `deepseek-v3.2-speciale`, `deepseek-v4-flash`, `deepseek-v4-pro` |
| MiniMax | [`configs/model-families/minimax.toml`](configs/model-families/minimax.toml) | `minimax-m2.5`, `minimax-m2.7`, `minimax-m3` |
| Moonshot AI | [`configs/model-families/moonshot-ai.toml`](configs/model-families/moonshot-ai.toml) | `kimi-k2`, `kimi-k2-0905`, `kimi-k2.5`, `kimi-k2.6`, `kimi-k2.6-code`, `kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-for-coding` |
| Alibaba Cloud | [`configs/model-families/qwen.toml`](configs/model-families/qwen.toml) | `qwen3.6-35b-a3b`; conservative broad defaults for `qwen3.6*` and `qwen3.7*` |
| xAI | [`configs/model-families/x-ai.toml`](configs/model-families/x-ai.toml) | `grok-4.3`, `grok-4.5`, `grok-build-0.1` |
| Xiaomi | [`configs/model-families/xiaomi.toml`](configs/model-families/xiaomi.toml) | `mimo-v2.5`, `mimo-v2.5-pro` |
| Z.ai | [`configs/model-families/z-ai.toml`](configs/model-families/z-ai.toml) | `glm-5`, `glm-5.1`, `glm-5.2` |
| Tencent Hunyuan 3 (Hy3) | [`configs/model-families/hy3.toml`](configs/model-families/hy3.toml) | `hy3`, `hy3:free`, `hicap/hy3`, `hicap/hy3:free`, `tencent/hy3`, `tencent/hy3:free` |

## More Docs

- [Quick start](docs/quick-start.md)
- [Contributing](CONTRIBUTING.md)
- [Configuration guide](docs/configuration.md)
- [Developer build guide](docs/development.md)
- [Provider catalogs](docs/provider-catalogs.md)
- [Model-family catalogs](docs/model-family-catalogs.md)
- [Codex client compatibility](docs/codex-cli-compatibility.md)
- [Tool approval policy](docs/tool-approval-policy.md)
- [Live testing](docs/live-testing.md)
- [Legal notices](docs/legal-notices.md)

## Current Scope

Implemented now:

- `POST /v1/responses` and `/responses`
- `GET /v1/models` and `/models`
- streaming chat-completions text to Responses SSE
- streaming chat-completions function calls to Responses `function_call` output
  items
- non-streaming chat-completions response conversion
- editable TOML provider headers, auth, endpoint paths, model metadata, and
  tool/request morphs
- opt-in continue guard for premature chat-completions stops during active
  Codex plans
- sanitized debug logging that redacts obvious API keys and provider tokens even
  when full bodies or raw stream frames are enabled
- opt-in tool approval policy for GitHub CLI approval hints, escalation
  requests without reusable prefixes, and token-disclosure blocking
- upstream `User-Agent` reporting as `codex-warp/<version>`
- GitHub Actions CI for format, tests, build, CLI smoke, and whitespace checks

Still intentionally small:

- no multimodal image/file request translation yet
- no namespace tool expansion beyond a simple fallback function
- no built-in provider profile auto-connects without user config

## Affiliation

Codex Warp is an independent project and is not affiliated with, endorsed by,
sponsored by, or approved by OpenAI. References to OpenAI, Codex, ChatGPT, the
OpenAI API, Responses API, or related product names are for descriptive
compatibility purposes only. Those names may be trademarks, service marks,
product names, or other protected names owned by OpenAI or its affiliates.

## License

Codex Warp is licensed under the Apache License 2.0 with the Commons Clause
License Condition v1.0. Personal use, internal business use, modification, and
distribution are allowed under the license terms, but selling or reselling the
software, including paid hosted services whose value substantially comes from
Codex Warp, requires a separate license from jatmn.

See [`NOTICE`](NOTICE) for attribution, non-affiliation, and trademark notices.
