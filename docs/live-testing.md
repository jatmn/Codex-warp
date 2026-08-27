# Live Testing

Use real provider keys only in your shell environment. Do not commit them.

The commands below use Bash on Linux and macOS. On Windows PowerShell, use the
[provider startup](quick-start.md#2-choose-a-provider),
[proxy checks](quick-start.md#3-check-the-proxy), and
[smoke test](quick-start.md#6-run-a-smoke-test) from the quick start, replacing
the provider key, profile path, and model ID with the values described here.

## Table Of Contents

- [Build](#build)
- [Start Codex Warp](#start-codex-warp)
- [Check Health](#check-health)
- [Check Model Catalog](#check-model-catalog)
- [Codex Smoke Test](#codex-smoke-test)
- [Local Validation](#local-validation)

## Build

```bash
cargo build
```

## Start Codex Warp

With the Xiaomi profile:

```bash
export XIAOMI_TOKEN_PLAN_API_KEY="..."
target/debug/codex-warp --config configs/xiaomi-token-plan.toml
```

With the Moonshot KimiCode profile:

```bash
export KIMICODE_API_KEY="..."
target/debug/codex-warp --config configs/moonshot-kimicode.toml
```

With Hicap:

```bash
export HICAP_API_KEY="..."
target/debug/codex-warp --config configs/hicap.toml
```

For the Codex smoke test below, use the bundled Hicap fallback model by
replacing `-m mimo-v2.5` with `-m hicap/glm-5.2`.

With a quick destination override:

```bash
target/debug/codex-warp --destination https://provider.example/v1
```

`--destination` only overrides the upstream URL. For authenticated providers,
use a Codex Warp provider config with `api_key_env`; upstream credentials belong
in Codex Warp, not in Codex's `model_providers.codex-warp` entry.

## Check Health

```bash
curl -sS http://127.0.0.1:8787/health
```

Expected:

```text
ok
```

## Check Model Catalog

```bash
curl -sS http://127.0.0.1:8787/v1/models
```

This confirms the proxy can reach the upstream provider and normalize the model
list for Codex.

## Codex Smoke Test

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

Codex may warn that provider-specific model metadata was fallback-resolved for
the selected model. The smoke test is successful when the command exits with
status 0 and the last-message file contains `hello`.

## Local Validation

Before ordinary local commits, new PR submission, and PR-update push, run the
[full local CI preflight](development.md#local-validation). For commit routes
that Git cannot prevent with a hook, follow the rebase, cherry-pick, and revert
workflow in that guide. The preflight is also required after a live smoke test
changes code or configuration:

```bash
bash scripts/ci-preflight.sh
```
