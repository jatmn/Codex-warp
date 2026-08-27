# Quick Start

This guide takes Codex Warp from a fresh checkout to a working Codex session
with one upstream provider.

## Before You Start

You need:

- Codex Desktop or Codex CLI
- Git and a stable Rust toolchain
- CMake and a C/C++ build toolchain
- an API key for an OpenAI-compatible provider

If you use Codex Desktop, the CLI is not required for normal sessions. The
smoke test in step 6 is an optional CLI-only verification.

See the [developer build guide](development.md) for exact Linux, macOS, and
Windows prerequisites.

## 1. Download And Build

```bash
git clone https://github.com/jatmn/Codex-warp.git
cd Codex-warp
cargo build --release
```

The resulting binary is:

- Linux and macOS: `target/release/codex-warp`
- Windows: `target\release\codex-warp.exe`

Keep the repository's `codex-warp.toml` and `configs/` directory available when
you run the binary. The runtime configuration is not embedded in the
executable.

## 2. Choose A Provider

Codex Warp includes ready-made profiles. Export the matching API key and pass
the profile with `--config`.

OpenRouter:

```bash
export OPENROUTER_API_KEY="..."
./target/release/codex-warp --config configs/openrouter.toml
```

Moonshot Kimi Code:

```bash
export KIMICODE_API_KEY="..."
./target/release/codex-warp --config configs/moonshot-kimicode.toml
```

Xiaomi Token Plan:

```bash
export XIAOMI_TOKEN_PLAN_API_KEY="..."
./target/release/codex-warp --config configs/xiaomi-token-plan.toml
```

On Windows PowerShell, set an environment variable for the current terminal
like this:

```powershell
$env:OPENROUTER_API_KEY = "..."
.\target\release\codex-warp.exe --config configs\openrouter.toml
```

For another gateway, copy
[`configs/openai-compatible.toml`](../configs/openai-compatible.toml), then set
its `base_url`, `api_key_env`, and endpoint behavior. Provider credentials
belong in Codex Warp, not in Codex's local-provider entry.

Warp starts on `http://127.0.0.1:8787` by default. Leave it running and use a
second terminal for the remaining steps.

## 3. Check The Proxy

Confirm that the process is healthy:

### Linux And macOS

```bash
curl -sS http://127.0.0.1:8787/health
# ok
```

Then inspect the model catalog:

```bash
curl -sS http://127.0.0.1:8787/v1/models
```

### Windows PowerShell

```powershell
Invoke-RestMethod http://127.0.0.1:8787/health
# ok

Invoke-RestMethod http://127.0.0.1:8787/v1/models
```

If the health check works but the model request fails, check the API key name,
the selected profile, and the terminal running Warp. For more diagnostic
output, set the debug environment variable before restarting Warp with the
selected profile:

```bash
export RUST_LOG=codex_warp=debug
```

```powershell
$env:RUST_LOG = "codex_warp=debug"
```

## 4. Configure Codex

Codex reads personal settings from `~/.codex/config.toml`. Add the following
provider definition:

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

The command prints a nonsecret placeholder bearer token for the local proxy.
Warp owns the real upstream credential through the provider profile's
`api_key_env`. Do not add the upstream key, `model_catalog_json`, or a
Codex-side `env_key` to this entry.

Warp's default `hide_codex_builtin_models = true` keeps Codex's bundled models
out of the gateway-only model picker. Change that setting in `codex-warp.toml`
if you want a mixed catalog.

### Windows Codex Auth

Windows does not provide `printf` by default. Use the zero-argument `hostname`
command instead; its output is only a nonsecret placeholder for the local
proxy:

```toml
[model_providers.codex-warp.auth]
command = "hostname"
refresh_interval_ms = 0
```

## 5. Use Codex

Restart Codex after changing its configuration. In Codex Desktop, reopen the
app and select one of the models returned by Warp's `/v1/models` endpoint. With
Codex CLI, select a returned model and start a session normally:

```bash
codex
```

Codex sends Responses-shaped requests to the local proxy. Warp selects the
configured gateway, applies provider and model-family transforms, and forwards
the adapted request upstream.

## 6. Run A Smoke Test

This optional check requires Codex CLI and ignores the rest of your user
configuration. Desktop-only users can skip it after completing step 5. Replace
`MODEL_ID_FROM_CATALOG` with a model ID returned by `/v1/models`:

### Linux And macOS

```bash
codex exec \
  --ignore-user-config \
  --skip-git-repo-check \
  -C /tmp \
  -m MODEL_ID_FROM_CATALOG \
  -c 'model_provider="codex-warp"' \
  -c 'model_providers.codex-warp.name="Codex Warp"' \
  -c 'model_providers.codex-warp.base_url="http://127.0.0.1:8787/v1"' \
  -c 'model_providers.codex-warp.wire_api="responses"' \
  -c 'model_providers.codex-warp.auth.command="printf"' \
  -c 'model_providers.codex-warp.auth.args=["codex-warp-local"]' \
  -c 'model_providers.codex-warp.auth.refresh_interval_ms=0' \
  -s read-only \
  --output-last-message /tmp/codex-warp-hello.txt \
  'Respond with exactly one word: hello'
```

Expected result:

```bash
cat /tmp/codex-warp-hello.txt
# hello
```

### Windows PowerShell

```powershell
$outputPath = Join-Path $env:TEMP "codex-warp-hello.txt"

codex exec `
  --ignore-user-config `
  --skip-git-repo-check `
  -C $env:TEMP `
  -m MODEL_ID_FROM_CATALOG `
  -c 'model_provider="codex-warp"' `
  -c 'model_providers.codex-warp.name="Codex Warp"' `
  -c 'model_providers.codex-warp.base_url="http://127.0.0.1:8787/v1"' `
  -c 'model_providers.codex-warp.wire_api="responses"' `
  -c 'model_providers.codex-warp.auth.command="hostname"' `
  -c 'model_providers.codex-warp.auth.refresh_interval_ms=0' `
  -s read-only `
  --output-last-message $outputPath `
  'Respond with exactly one word: hello'

Get-Content $outputPath
# hello
```

See [live testing](live-testing.md) for provider-specific checks and failure
diagnosis.

## Next Steps

- Load multiple gateways: [configuration guide](configuration.md)
- Add a custom gateway: [provider catalogs](provider-catalogs.md)
- Tune model capabilities: [model-family catalogs](model-family-catalogs.md)
- Enable the management UI: [Web UI and analytics](configuration.md#web-ui-and-analytics)
- Configure tool-call rules: [tool approval policy](tool-approval-policy.md)
