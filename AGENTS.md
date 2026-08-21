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
- [Common Bug Classes](#common-bug-classes)
- [Development](#development)
- [Self-Review Gates](#self-review-gates)
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
- `src/namespace_helpers.rs`: Codex namespace-tool expansion for sub-agent helpers
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

### Test Quality

New branches and bug fixes need a test that would fail without the change.
Do not add extra tests, snapshots, or fixtures just to look thorough.
CI mutation testing fails when a change can be mutated without a test noticing.
The workflow runs
`cargo mutants --no-shuffle -vV --in-diff git.diff -- --locked`
on the PR Rust diff. If mutants fail, strengthen the assertion on the behavior
you meant to protect. Do not add a second test that only duplicates coverage
theater.

### Required Local CI Preflight

Before **every local commit**, every new PR submission, and every push that
updates an existing PR, run the full local Linux preflight:

```bash
bash scripts/ci-preflight.sh
```

For a PR whose base is not `main`, run
`bash scripts/ci-preflight.sh --base origin/<base-branch>` instead. This is
required; do not bypass Git hooks with `--no-verify` or treat remote CI as the
first execution of these checks. Install the durable hook bootstrap once per
checkout; it dispatches to this checkout's versioned hook and preflight
implementation:

```bash
bash scripts/install-git-hooks.sh
```

After updating from an earlier hook installation, run the installer once again
to migrate the hook path. If a checkout moves to a branch that does not provide
the preflight scripts, the installed hook fails closed instead of silently
allowing the commit or push.
The installer chains pre-existing custom and default Git hooks rather than
replacing them.

The installed hooks automatically cover ordinary commits, non-fast-forward
merge commits, `git am`, and branch pushes. Git offers no preventative hook for
a bare `git cherry-pick`; use `git cherry-pick --no-commit <commit>`, run the
preflight, then create the commit so the check runs before the result is
recorded. The pre-push hook remains a backstop for any commit that reaches a
branch push.

For a non-`main` PR base, configure the hooks with the same base once:

```bash
bash scripts/install-git-hooks.sh --base origin/<base-branch>
```

`scripts/ci-preflight.sh` explicitly runs the non-Windows CI gates in this
order: `cargo update --workspace --locked`; `typos`;
`SOURCE_CHECKS_SKIP_TYPOS=1 bash scripts/source-checks.sh` (rustfmt, docs
whitespace/prose, Web UI JavaScript, chart harness, and crate-wide Clippy);
`cargo test --locked`; `cargo build --locked`;
`RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps`; CLI `--version` and
`--help` smoke checks; `git diff --check`; conditional Rust-diff
`cargo mutants -o <temporary-dir> --no-shuffle -vV --in-diff ... -- --locked`;
`cargo deny check bans licenses sources`; and `cargo audit`. The Windows job is
the sole excluded CI check. `cargo audit` still runs locally, but its advisory
result is non-blocking because CI marks it `continue-on-error`.

When asserting JSON or other structured values, check that the field exists.
Do not hide a missing key with `unwrap_or(0)`, `unwrap_or("")`, or similar
defaults in tests. Use `get` plus `assert!`/`unwrap` on the option, or match
the exact `Value`:

```rust
let input = body
    .get("usage")
    .and_then(|usage| usage.get("input_tokens"))
    .and_then(Value::as_u64)
    .expect("usage.input_tokens");
assert_eq!(input, 3);
```

Reuse existing sanitization and fixture helpers instead of copying a new
redaction path. Do not introduce process-wide `env::set_var` / `env::remove_var`
in tests; they race and leak across cases. Thread config, temp files, or
explicit function arguments instead, for example
`fn apply(config: &DebugConfig)` or a `tempfile` path, never
`std::env::set_var("RUST_LOG", ...)`.

Do not use `innerHTML` in the Web UI. Keep the existing DOM APIs
(`textContent`, `createElement`, and the current icon helper).

Do not weaken assertions (`unwrap_or` on required fields, `is_ok()` without
checking the value, constant `assert!(true)`) to make a test pass.

## Common Bug Classes

These come up in this repo. Fix the class, not only the one call site, when
you touch the area:

- Provider quirks belong in TOML catalogs and morphs, not new Rust special
  cases, unless the runtime needs a generic capability.
- Secrets and API keys in logs, Web UI, or tests must stay redacted previews.
  Never commit real credentials or dump full tokens in fixtures.
- Web UI XSS: no `innerHTML` for untrusted or interpolated strings.
- Tests must not use process-wide environment mutation.
- Response/chat conversion and usage fields: assert the JSON shape you care
  about. A default of zero is not proof the proxy emitted the field.

## Development

For a focused code-change feedback loop before the required local CI preflight,
run:

```bash
bash scripts/source-checks.sh
cargo test --locked
cargo build --locked
git diff --check
```

The chart harness exercises `chart-math.js` policy (ticks, hover identity, keyboard
ownership, pointer reclaim only on hit, paint only with a measured CSS width,
live-region clear, canvas interactivity attrs, bar paint anchors) and
`footer-status.js` (analytics footer copy when chart-math is missing, boot
errors skipping that overlay). It is not a browser canvas stub of `app-main.js`.

For a focused documentation-only feedback loop before the required local CI
preflight:

```bash
SOURCE_CHECKS_CLIPPY=0 bash scripts/source-checks.sh
git diff --check
```

## Self-Review Gates

These checks exist so the first local review finds nits instead of dripping
them into the next fix/push/review round. They do not replace review. A later
reviewer (or Cubic/Sourcery) finding a typo, capitalization nit, or Clippy
warning in a file you changed means this pass was skipped or incomplete.

Before you call implementation or a local review done:

1. Run `bash scripts/source-checks.sh`. Fix every failure (`cargo fmt`, `typos`,
   trailing whitespace, lowercase docs contractions such as `i'll`, JavaScript
   syntax, chart harness).
2. Read the Clippy output from that script. The script fails on any Clippy
   warning (`-D warnings`). Those are defects to fix or an explicit, justified
   `allow` with a comment. Do not leave them for the next review round. Do not
   expand crate-level Clippy allows to silence one call site.
3. Inspect changed comments, docs, user-visible strings, and test fixtures for
   spelling, grammar, and capitalization. `typos` misses some prose nits
   (contractions, title case). Those are still findings.
4. After each fix round, re-run the script before starting another review.
   Do not ping Cubic/Sourcery or re-run a full AI review until the mechanical
   gates are green and crate-wide Clippy is clean.

Install the spell checker with `cargo install typos-cli --locked` (Rust, not
Python). Add `_typos.toml` exceptions only for confirmed identifiers or
fixtures, never to hide a real misspelling.

Use `apply_patch` for manual edits. Avoid unrelated refactors while fixing a
specific compatibility issue.

Do not drive-by reformat files, rename identifiers, expand Clippy allows, or
add `_typos.toml` words to silence a gate. If a gate fails, fix the underlying
issue or document a justified, local `allow` with a comment.

Do not bump `Cargo.toml` version. Do not add Python. Do not add extra CI jobs,
coverage percentages, or new linters in a feature PR. Those belong in their
own stacked CI PRs.

## Secrets And Live Providers

Do not commit API keys. Prefer `api_key_env` in provider profiles and export the
key locally when testing.

The Xiaomi Token Plan profile uses `XIAOMI_TOKEN_PLAN_API_KEY`. See
[`docs/live-testing.md`](docs/live-testing.md) for the current
manual smoke-test flow.

## Commits

When asked to commit local work, use a descriptive subject and a body that
summarizes the functional changes and validation commands that were run.
