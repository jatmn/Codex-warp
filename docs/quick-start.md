# Quick Start

This guide gets Codex Warp running locally with one upstream provider.

## Table Of Contents

- [1. Build](#1-build)
- [2. Choose A Provider](#2-choose-a-provider)
- [3. Configure Codex](#3-configure-codex)
- [4. Check Models](#4-check-models)
- [5. Smoke Test](#5-smoke-test)

## 1. Build

```bash
cargo build
```

The binary will be at:

```bash
target/debug/codex-warp
```

## 2. Choose A Provider

Use an existing profile:

```bash
export XIAOMI_TOKEN_PLAN_API_KEY="..."
target/debug/codex-warp --config configs/xiaomi-token-plan.toml
```

Or point at a provider quickly without editing config:

```bash
target/debug/codex-warp --destination https://provider.example/v1
```

`--destination` only overrides the upstream URL. For providers that need an API
key, use a Codex Warp provider config with `api_key_env`; upstream credentials
belong in Codex Warp, not in Codex's `model_providers.codex-warp` entry.

For a reusable custom provider, copy or edit
[`configs/openai-compatible.toml`](../configs/openai-compatible.toml).

## 3. Configure Codex

Point Codex at the local proxy as a Responses provider:

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

Codex Warp uses its own provider config for upstream keys. Do not set
`model_catalog_json` or a Codex-side `env_key` on the `codex-warp` provider:
the `auth` block above is a local refresh shim that makes current Codex CLI
auto-populate models from the proxy's `/v1/models` endpoint. Provider
credentials still belong only in Codex Warp gateway configs.

Warp's default `[config] hide_codex_builtin_models = true` keeps Codex's bundled
GPT models out of that auto-populated picker. Leave it enabled for Codex Warp
gateway-only instances.

## 4. Check Models

In another terminal:

```bash
curl -sS http://127.0.0.1:8787/v1/models
```

When a provider returns a plain OpenAI-compatible list such as
`{"object":"list","data":[{"id":"mimo-v2.5"}]}`, Codex Warp synthesizes the
richer Codex model metadata needed by Codex CLI.

## 5. Smoke Test

Run a one-word Codex check:

```bash
codex exec \
  --ignore-user-config \
  --skip-git-repo-check \
  -C /tmp \
  -m mimo-v2.5 \
  -c 'model_provider="codex-warp"' \
  -c 'model_providers.codex-warp.name="Codex Warp"' \
  -c 'model_providers.codex-warp.base_url="http://127.0.0.1:8787/v1"' \
  -c 'model_providers.codex-warp.wire_api="responses"' \
  -c 'model_providers.codex-warp.requires_openai_auth=false' \
  -c 'model_providers.codex-warp.auth.command="printf"' \
  -c 'model_providers.codex-warp.auth.args=["codex-warp-local"]' \
  -c 'model_providers.codex-warp.auth.refresh_interval_ms=0' \
  -s read-only \
  --output-last-message /tmp/codex-warp-hello.txt \
  'Respond with exactly one word: hello'
```

Expected:

```bash
cat /tmp/codex-warp-hello.txt
# hello
```

See [live testing](live-testing.md) for a fuller checklist.
