# Provider Catalogs

Provider catalogs, also called gateway profiles, tell Codex Warp how to connect
to an upstream API gateway. They should describe gateway behavior only:
endpoints, auth, headers, and provider-specific corrections. Model behavior
belongs in [model-family catalogs](model-family-catalogs.md)
unless a particular gateway reports a model differently.

## Table Of Contents

- [Included Provider Profiles](#included-provider-profiles)
- [Provider Profile Shape](#provider-profile-shape)
- [Fields](#fields)
- [Adding A New Gateway Provider](#adding-a-new-gateway-provider)
- [When To Use Provider Metadata Overrides](#when-to-use-provider-metadata-overrides)
- [Provider-Specific Transforms](#provider-specific-transforms)
- [Multiple Providers](#multiple-providers)
- [Testing A Provider Profile](#testing-a-provider-profile)

## Included Provider Profiles

| Profile | File | Use |
| --- | --- | --- |
| OpenAI-compatible template | [`configs/openai-compatible.toml`](../configs/openai-compatible.toml) | Starting point for any generic OpenAI-compatible provider. |
| ClinePass | [`configs/clinepass.toml`](../configs/clinepass.toml) | ClinePass profile with a local catalog from the public docs. |
| Moonshot KimiCode | [`configs/moonshot-kimicode.toml`](../configs/moonshot-kimicode.toml) | KimiCode subscription profile for Moonshot's OpenAI-compatible API shape, with a local Kimi catalog fallback. |
| OpenCode Go | [`configs/opencode-go.toml`](../configs/opencode-go.toml) | OpenCode Go subscription profile, limited to the documented OpenAI-compatible chat-completions models. |
| Xiaomi Token Plan | [`configs/xiaomi-token-plan.toml`](../configs/xiaomi-token-plan.toml) | Ready profile for `https://token-plan-sgp.xiaomimimo.com/v1`. |

No provider profile is enabled by default. The baseline config loads model
families, but it intentionally does not auto-connect to any upstream gateway.

## Provider Profile Shape

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

[provider.headers]
"X-Provider-Feature" = "enabled"
```

Named provider:

```toml
[providers.provider_a]
name = "Provider A"
base_url = "https://provider-a.example/v1"
api_key_env = "PROVIDER_A_API_KEY"
auth_header = "authorization"
auth_scheme = "Bearer"
responses_path = "/responses"
chat_completions_path = "/chat/completions"
models_path = "/models"
model_catalog_only = false

[providers.provider_a.headers]
"HTTP-Referer" = "https://example.local"
"X-Title" = "Codex Warp"
```

Use named providers when you want Codex Warp to merge more than one upstream
model catalog. Codex Warp groups the merged `/v1/models` response by gateway
and prefixes display names with `[name]`, for example `[Provider A] Model`.

## Fields

| Field | Meaning |
| --- | --- |
| `name` | Friendly gateway label shown as a prefix in Codex model display names. Falls back to the provider id. |
| `base_url` | Upstream API root. Usually ends in `/v1`. Empty means disabled. |
| `api_key_env` | Environment variable used for upstream auth. Preferred for secrets. |
| `api_key` | Inline upstream key. Useful for local experiments, but avoid committing it. |
| `auth_header` | Header used for auth. Defaults to `authorization`. |
| `auth_scheme` | Prefix for the key. Defaults to `Bearer`; set to `""` for raw keys. |
| `headers` | Static extra headers required by the gateway. `User-Agent` is ignored here because Codex Warp always reports itself as `codex-warp/<version>`. |
| `responses_path` | Upstream Responses endpoint path. |
| `chat_completions_path` | Upstream chat completions endpoint path. |
| `models_path` | Upstream model catalog endpoint path. |
| `model_catalog` | Optional local model list for providers that do not expose a usable `/models` endpoint. |
| `model_catalog_only` | When `true`, skip the upstream `/models` fetch and expose only local `model_catalog` entries. |
| `model_catalog.upstream_id` | Optional upstream model id to send when the Codex-facing catalog id is namespaced. |
| `model_metadata` | Gateway-specific model defaults or exact overrides. |
| `transform` | Gateway-specific request/tool translation overrides. |

## Adding A New Gateway Provider

1. Copy [`configs/openai-compatible.toml`](../configs/openai-compatible.toml).
2. Rename the provider id from `manual` to a stable id, for example
   `acme_ai`.
3. Set `base_url`.
4. Set `api_key_env` and document the environment variable.
5. Add any required custom headers.
6. Adjust endpoint paths if the gateway does not use OpenAI defaults.
7. Add `[[providers.<id>.model_catalog]]` entries if the gateway does not
   expose a usable `/models` endpoint.
8. Keep model-specific context, modality, reasoning, and tool behavior out of
   the provider profile when it is shared by that model across providers.
9. Add the file to the `include` list under `[config]` in `codex-warp.toml`,
   or pass it with `--config`.
10. Start the proxy and check `/v1/models`.

Example:

```toml
[providers.acme_ai]
base_url = "https://api.acme.example/v1"
api_key_env = "ACME_AI_API_KEY"
auth_header = "authorization"
auth_scheme = "Bearer"
responses_path = "/responses"
chat_completions_path = "/chat/completions"
models_path = "/models"
model_catalog_only = true

[providers.acme_ai.headers]
"X-Title" = "Codex Warp"

[[providers.acme_ai.model_catalog]]
id = "acme/model-a"
upstream_id = "model-a"
display_name = "Acme Model A"
```

Run it:

```bash
export ACME_AI_API_KEY="..."
target/debug/codex-warp --config configs/acme-ai.toml
```

## When To Use Provider Metadata Overrides

Use provider metadata overrides only when the gateway's catalog is sparse or
wrong for that gateway.

```toml
[providers.acme_ai.model_metadata.overrides."acme-model"]
context_window = 200000
input_modalities = ["text", "image"]
```

Set `auto_review_model_override` in model-family metadata whenever the review
model is a family rule. Use the lower model in that family when one exists, or
point the family at itself when it is the only available family member.

Provider catalogs may use gateway-specific prefixes such as
`cline-pass/kimi-k2.7-code`. Keep the family override unprefixed, such as
`kimi-k2.6`; Codex Warp resolves it to a matching model in the same provider
catalog before returning the model list to Codex. If the target is not present in
that provider catalog, Warp falls back to the current model instead of crossing
providers.

If the same metadata applies everywhere that model appears, add it to a
model-family catalog instead.

## Provider-Specific Transforms

Most OpenAI-compatible chat gateways can inherit the baseline transform. Add a
provider transform only when the gateway itself needs different behavior.

```toml
[providers.acme_ai.transform]
backend = "open_ai_chat"
unsupported_tool_types = ["custom"]
unsupported_tool_strategy = "as_function"
drop_empty_tool_choice = true
```

For native Responses gateways:

```toml
[providers.acme_ai.transform]
backend = "responses"
unsupported_tool_strategy = "passthrough"
```

Model-specific transform quirks should usually live in model-family catalogs.

## Multiple Providers

You can include several provider profiles at once:

```toml
[config]
include = [
  "configs/provider-a.toml",
  "configs/provider-b.toml",
]
```

Codex Warp merges successful model catalogs from all enabled providers. When
duplicate model slugs exist, the first provider to report that slug wins the
route.

## Testing A Provider Profile

1. Start the proxy with the provider config.
2. Check health:

   ```bash
   curl -sS http://127.0.0.1:8787/health
   ```

3. Check the merged model catalog:

   ```bash
   curl -sS http://127.0.0.1:8787/v1/models
   ```

4. Run the Codex smoke test from
   [live testing](live-testing.md).
5. If the provider rejects a field, prefer a config morph over changing code
   when the behavior is provider-specific or model-specific.
