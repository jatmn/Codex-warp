# AGENTS.md

Guidance for coding agents working in this repository.

## Table Of Contents

- [Project](#project)
- [Important Docs](#important-docs)
- [Repo Layout](#repo-layout)
- [Config Boundaries](#config-boundaries)
- [Catalog Rules](#catalog-rules)
- [Versioning](#versioning)
- [Source Layout](#source-layout)
- [Testing Layout](#testing-layout)
- [Development](#development)
- [Secrets And Live Providers](#secrets-and-live-providers)
- [Commits](#commits)

## Project

Codex Warp is a small Rust proxy that exposes a local Responses API surface for
Codex Desktop, Codex CLI, and other Codex clients, then translates requests to
one or more upstream OpenAI-compatible providers.

Keep the project lightweight. Prefer editable TOML config for provider and
model quirks instead of hard-coding compatibility data in Rust unless the
runtime needs a new generic capability.

## Important Docs

- Start with [`README.md`](README.md) for project scope.
- Use [`docs/configuration.md`](docs/configuration.md) for config
  merge behavior and request morphs.
- Use [`docs/provider-catalogs.md`](docs/provider-catalogs.md) when
  adding or changing gateway/provider profiles.
- Use [`docs/model-family-catalogs.md`](docs/model-family-catalogs.md)
  when adding model brands, model families, exact model overrides, context
  windows, reasoning behavior, modalities, or tool support.
- Use [`docs/live-testing.md`](docs/live-testing.md) for Codex CLI
  smoke tests against a live upstream.
- Use [`docs/development.md`](docs/development.md) for platform
  build instructions.

## Repo Layout

- `src/`: Rust proxy implementation.
- `codex-warp.toml`: baseline config, loaded first.
- `configs/`: provider profiles and reusable compatibility config.
- `configs/model-families/`: model-family catalogs loaded by the baseline
  config.
- `configs/tool-policies/`: optional tool approval policy rule sets loaded by
  the baseline config but disabled until `[tool_policy].enabled = true`.
- `docs/`: user and developer documentation.

## Config Boundaries

Provider profiles describe gateway behavior:

- base URLs
- endpoint paths
- auth and static headers
- gateway-specific metadata corrections
- gateway-specific request/tool transforms

Model-family catalogs describe model behavior that follows the model across
providers:

- context windows
- modalities and vision support
- reasoning levels and thinking transforms
- search support
- parallel tool-call support
- model-specific tool quirks

Do not duplicate common OpenAI-compatible transforms into provider profiles.
Do not put API keys, auth headers, provider URLs, or endpoint paths in
model-family catalogs.

## Catalog Rules

- Default context should stay conservative unless a model catalog or provider
  override supplies a real value.
- Broad model-family entries should contain only behavior shared by every model
  matched by the pattern.
- Use exact model entries for different context windows, reasoning modes,
  search support, modality support, or tool behavior.
- Prefer `priority = 0` for broad family defaults and `priority = 10` for exact
  model overrides.
- Include common aliases where providers differ, such as dotted, dashed, and
  underscored model ids.
- If a provider rejects a Codex field, translate it when possible. Dropping a
  field should be intentional and documented by the transform.

## Versioning

Codex Warp reports itself to upstream providers as `codex-warp/<version>`.
The version comes from `Cargo.toml` through `src/version.rs`.

Do not automatically increment the version during normal agent work, even when
committing feature or fix changes. Version bumps are controlled by the GitHub
release worker only.

## Source Layout

Keep source changes near their domain:

- `src/server.rs`: CLI parsing, startup, routes, and top-level handlers
- `src/state.rs`: shared request state and selected-provider structs
- `src/provider.rs`: provider selection and provider display names
- `src/config.rs`: TOML config schema and defaults
- `src/config_loader.rs`: config includes, TOML merging, and provider lookup
- `src/models.rs`: `/models` catalog fetching, sorting, and metadata shaping
- `src/upstream.rs`: upstream request dispatch and response plumbing
- `src/response_codec.rs`: SSE, chat/responses conversion, usage normalization
- `src/store.rs`: SQLite overlays and usage analytics
- `src/structured_output.rs`: Chat Completions JSON Schema compatibility fallback
- `src/guardian_compat.rs`: Guardian auto-review prompt compatibility shim
- `src/webui.rs`: embedded Web UI routes and config/analytics API
- `src/provider_templates.rs`: bundled example provider profiles for the Web UI
- `src/webui_static/`: embedded HTML/CSS/JS for the Web UI
- `src/transform_morph.rs`: configured request morphs and dotted-path edits
- `src/tool_policy.rs`: optional downstream tool-call approval policy
- `src/debug_log.rs`: sanitized debug log events and fingerprints
- `src/process_log.rs`: in-memory process log buffer and tracing filter reload
- `src/http.rs`: shared HTTP headers, endpoint URLs, and proxy errors
- `src/ids.rs`: generated Responses item/call ids
- `src/version.rs`: agent name and version reporting

## Testing Layout

Keep production modules focused. Put unit tests in sibling test files named
`src/<module>_tests.rs` and include them from the production module with
`#[cfg(test)]` plus `#[path = "..."]`.

Small, highly local tests may live near the code only when that improves
readability more than a separate test file. Avoid adding new large inline
`mod tests` blocks to production modules.

## Development

Run these for code changes:

```bash
cargo fmt --check
cargo test --locked
cargo build --locked
git diff --check
```

For documentation-only changes:

```bash
git diff --check
rg -n "[ \t]+$" README.md AGENTS.md docs
```

Use `apply_patch` for manual edits. Avoid unrelated refactors while fixing a
specific compatibility issue.

## Secrets And Live Providers

Do not commit API keys. Prefer `api_key_env` in provider profiles and export the
key locally when testing.

The Xiaomi Token Plan profile uses `XIAOMI_TOKEN_PLAN_API_KEY`. See
[`docs/live-testing.md`](docs/live-testing.md) for the current
manual smoke-test flow.

## Commits

When asked to commit local work, use a descriptive subject and a body that
summarizes the functional changes and validation commands that were run.
