<div align="center">

# Codex Warp

**Use OpenAI-compatible models and gateways with Codex.**

[![CI](https://github.com/jatmn/Codex-warp/actions/workflows/ci.yml/badge.svg)](https://github.com/jatmn/Codex-warp/actions/workflows/ci.yml)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-Apache--2.0%20%2B%20Commons%20Clause-blue.svg)](LICENSE)

[Quick start](docs/quick-start.md) · [Configuration](docs/configuration.md) · [Releases](docs/releases.md) · [Contributing](CONTRIBUTING.md)

</div>

Codex Warp is a small, local Rust proxy for Codex Desktop, Codex CLI, and other
Codex clients. It exposes the Responses API that Codex expects and translates
requests for providers that implement OpenAI-compatible Chat Completions or
partial Responses support.

```text
Codex Desktop / CLI  ──Responses API──▶  Codex Warp  ──provider API──▶  Model gateway
                                          │
                                          └── TOML compatibility rules
```

Provider quirks live in editable TOML instead of client patches or hard-coded
forks. Warp adapts tools, streaming events, model metadata, reasoning fields,
and other request differences on the way through.

## Get Started

You need [Rust](https://rustup.rs/), CMake, a C/C++ build toolchain, Codex, and
an API key for an upstream provider. The commands below use OpenRouter as an
example; other ready-made profiles are listed under
[`configs/`](configs/).

### 1. Build Codex Warp

```bash
git clone https://github.com/jatmn/Codex-warp.git
cd Codex-warp
cargo build --release
```

Versioned official archives and immutable nightly prereleases are available
from [GitHub Releases](https://github.com/jatmn/Codex-warp/releases). See
[Releases](docs/releases.md) for channel, asset, checksum, and provenance
details.

For platform-specific prerequisites, see the
[Linux, macOS, and Windows build guide](docs/development.md).

### 2. Start A Provider

The condensed commands below use Bash on Linux and macOS. On Windows
PowerShell, follow the full quick start through
[provider startup](docs/quick-start.md#2-choose-a-provider),
[proxy checks](docs/quick-start.md#3-check-the-proxy), and
[Windows Codex auth](docs/quick-start.md#windows-codex-auth), then continue at
[Use Codex](docs/quick-start.md#5-use-codex).

```bash
export OPENROUTER_API_KEY="..."
./target/release/codex-warp --config configs/openrouter.toml
```

Warp listens on `http://127.0.0.1:8787` by default. Check it from another
terminal:

```bash
curl -sS http://127.0.0.1:8787/health
# ok
```

### 3. Point Codex At Warp

Add this to `~/.codex/config.toml`:

```toml
model_provider = "codex-warp"

[model_providers.codex-warp]
name = "Codex Warp"
base_url = "http://127.0.0.1:8787/v1"
wire_api = "responses"

[model_providers.codex-warp.auth]
command = "printf"
args = ["codex-warp-local"]
refresh_interval_ms = 0
```

The command-backed token is only a local placeholder. Your real provider key
stays in Codex Warp's environment and is never added to the Codex provider
entry.

Restart Codex, select a model exposed by your gateway, and use Codex normally.
For Codex Desktop, fully quit and reopen the app so its managed app-server
daemon rebuilds the model manager. Warp serves the live model catalog at
`http://127.0.0.1:8787/v1/models`.

> [!NOTE]
> The `printf` token command is for Linux and macOS. The
> [full quick start](docs/quick-start.md#windows-codex-auth) includes a Windows
> equivalent, custom-provider setup, and a smoke test.

## Why Codex Warp?

- **Responses compatibility** — translates Responses requests to
  Chat Completions when a gateway does not implement the full Responses API.
- **Config-driven fixes** — request, tool, and provider behavior is controlled
  with composable TOML profiles.
- **Live model discovery** — merges upstream catalogs with local metadata so
  Codex receives model names, context limits, reasoning modes, modalities, and
  tool capabilities.
- **Agent-session resilience** — converts streaming and non-streaming tool
  calls, expands namespace tools used by subagents, and can recover from
  premature text-only stops.
- **Local operations** — optional Web UI, SQLite usage analytics, process logs,
  and sanitized debug events stay on your machine.
- **Policy controls** — optional TOML rules can attach approval hints, request
  escalation without a reusable prefix, or deny selected downstream tool calls.

## Provider Profiles

| Provider | Profile | API key variable |
| --- | --- | --- |
| ClinePass | [`configs/clinepass.toml`](configs/clinepass.toml) | `CLINEPASS_API_KEY` |
| Hicap | [`configs/hicap.toml`](configs/hicap.toml) | `HICAP_API_KEY` |
| Moonshot Kimi Code | [`configs/moonshot-kimicode.toml`](configs/moonshot-kimicode.toml) | `KIMICODE_API_KEY` |
| OpenCode Go | [`configs/opencode-go.toml`](configs/opencode-go.toml) | `OPENCODE_GO_API_KEY` |
| OpenRouter | [`configs/openrouter.toml`](configs/openrouter.toml) | `OPENROUTER_API_KEY` |
| Xiaomi Token Plan | [`configs/xiaomi-token-plan.toml`](configs/xiaomi-token-plan.toml) | `XIAOMI_TOKEN_PLAN_API_KEY` |
| Any OpenAI-compatible provider | [`configs/openai-compatible.toml`](configs/openai-compatible.toml) | You choose |

Profiles are disabled until you pass one with `--config` or include it from
`codex-warp.toml`. You can load multiple providers and route models by provider
prefix. See [provider catalogs](docs/provider-catalogs.md) for the profile
format and [configuration](docs/configuration.md) for merge and routing rules.

Model-family catalogs currently cover DeepSeek, MiniMax, Moonshot AI, Qwen,
xAI, Xiaomi, Z.ai, and Tencent Hunyuan 3. See
[`configs/model-families/`](configs/model-families/) for exact model entries.

## OpenRouter App Attribution

Warp automatically adds OpenRouter app-attribution headers when a configured
destination is `openrouter.ai` or one of its subdomains, including regional API
hosts:

- `HTTP-Referer`: `https://github.com/jatmn/Codex-warp`
- `X-OpenRouter-Title`: `Codex Warp`
- `X-Title`: `Codex Warp` (compatibility alias)
- `X-OpenRouter-Categories`: `cli-agent,programming-app`

These headers identify Codex Warp only for requests sent through OpenRouter.
To attribute traffic to your own project, override any of these values under
the OpenRouter profile's `[provider.headers]` or `[providers.<id>.headers]`
table. Explicitly configured headers take precedence over the defaults.

## Optional Web UI

Enable the local management UI in `codex-warp.toml`:

```toml
[webui]
enabled = true
```

Then open `http://127.0.0.1:8787/ui/` to manage providers and models, inspect
usage analytics, and view logs. Remote binding has additional authentication
requirements; read [Web UI and analytics](docs/configuration.md#web-ui-and-analytics)
before exposing it beyond localhost.

## Documentation

| If you want to... | Read... |
| --- | --- |
| Complete the first setup | [Quick start](docs/quick-start.md) |
| Configure providers, routing, transforms, logging, or the Web UI | [Configuration guide](docs/configuration.md) |
| Add or update a gateway profile | [Provider catalogs](docs/provider-catalogs.md) |
| Add model metadata and capabilities | [Model-family catalogs](docs/model-family-catalogs.md) |
| Understand Codex client behavior | [Codex compatibility](docs/codex-cli-compatibility.md) |
| Configure tool-call approval rules | [Tool approval policy](docs/tool-approval-policy.md) |
| Test against a live upstream | [Live testing](docs/live-testing.md) |
| Download or verify official and nightly builds | [Releases](docs/releases.md) |
| Build or contribute | [Development guide](docs/development.md) and [contributing](CONTRIBUTING.md) |

## Scope

Codex Warp currently handles `/v1/responses`, `/v1/models`, streaming and
non-streaming Chat Completions conversion, function and namespace tool calls,
structured output fallbacks, configurable request morphs, and local provider
management. Multimodal image and file request translation is not implemented.

Built-in profiles stay disabled until you select them explicitly. Selecting a
profile allows Warp to contact its configured upstream, so set any required
credential before starting Warp.

## Project Status And Safety

Codex Warp is an independent project and is not affiliated with, endorsed by,
sponsored by, or approved by OpenAI. Provider APIs and Codex compatibility can
change; review configuration changes and keep credentials out of checked-in
files.

Tool approval policy changes what Codex is told to approve, prompt for, or
block. Review every rule before enabling it. You are responsible for your own
policy configuration and use it at your own risk.

## License

Codex Warp is licensed under the Apache License 2.0 with the Commons Clause
License Condition v1.0. Personal use, internal business use, modification, and
distribution are allowed under the license terms. Selling or reselling the
software, including paid hosted services whose value substantially comes from
Codex Warp, requires a separate license from jatmn.

See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE) for the complete terms,
attribution, non-affiliation, and trademark notices.
