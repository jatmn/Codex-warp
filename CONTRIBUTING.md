# Contributing

Contributions are welcome. Codex Warp is intentionally small and direction-led,
so not every pull request will be accepted. A declined PR does not mean the work
is bad or unwelcome; it may simply not match the project's current scope,
maintenance budget, security posture, or technical direction.

## Before Opening A Pull Request

Before you start or submit work:

1. Search existing issues and pull requests.
2. Check for duplicate or overlapping open pull requests.
3. Read the relevant docs:
   - [`README.md`](README.md) for project scope.
   - [`docs/configuration.md`](docs/configuration.md) for configuration
     behavior.
   - [`docs/provider-catalogs.md`](docs/provider-catalogs.md) for provider
     profile changes.
   - [`docs/model-family-catalogs.md`](docs/model-family-catalogs.md) for model
     metadata and model-family changes.
   - [`docs/development.md`](docs/development.md) for build and validation
     commands.
4. Keep the change focused. Small, reviewable PRs are much easier to accept.

Duplicate PRs may be closed so review effort can stay concentrated in one
place.

## Project Direction

Good contributions usually fit one of these categories:

- Fixing correctness bugs in the proxy.
- Improving provider compatibility through config-driven behavior.
- Adding or correcting model-family metadata with evidence.
- Improving tests, docs, or safety around existing features.
- Reducing maintenance risk without expanding the project's scope.

Changes may be declined when they add too much surface area, move the project in
a different direction, duplicate existing work, introduce avoidable maintenance
cost, or solve a problem better handled outside Codex Warp.

## Language Policy

Do not introduce a new implementation language to this project.

Codex Warp is a Rust project with TOML configuration and Markdown
documentation. Python is strictly forbidden in this repository for any reason,
including scripts, tests, build tooling, generators, one-off utilities, or CI
helpers.

If a task seems to require Python, discuss the need first. The expected answer
will usually be Rust, shell, existing Cargo tooling, or no new tool at all.

## Pull Request Expectations

Every PR must:

- Explain what changed and why.
- Link related issues or prior discussion when available.
- Confirm that you checked for duplicate existing PRs.
- Keep secrets, API keys, provider tokens, private logs, and local machine paths
  out of the diff.
- Include tests or focused validation appropriate to the change.
- Pass CI before review and after every update.

Test-quality rules (one failing test per change, JSON field assertions, no
process-wide environment mutation) live in [`AGENTS.md`](AGENTS.md). Do not
restate or invent extra rules here.

You are responsible for keeping your PR green. If review feedback requires new
commits, re-run the relevant checks after those commits and make sure CI passes
again.

For code changes, run:

```bash
bash scripts/source-checks.sh
cargo test --locked
cargo build --locked
git diff --check
```

`scripts/source-checks.sh` is the mechanical review gate: rustfmt, `typos`
spelling, docs whitespace, docs contraction capitalization, Web UI JavaScript
syntax, the chart harness, and Clippy on added or edited Rust lines. Treat
those Clippy hits as review findings, not as optional style. Install `typos`
with `cargo install typos-cli --locked`.

For documentation-only changes, run:

```bash
SOURCE_CHECKS_CLIPPY=0 bash scripts/source-checks.sh
git diff --check
```

Some provider changes also need live validation against the affected upstream.
Use [`docs/live-testing.md`](docs/live-testing.md) when real provider behavior
is part of the change.

## Review And Follow-Up

Re-run `bash scripts/source-checks.sh` after every fix commit and before
requesting another AI or human review. Mechanical nits (spelling, rustfmt,
docs capitalization, Clippy on added or edited Rust lines) belong in that pass, not in
round two.

Please stay engaged after opening a PR.

Fly-by PRs with no follow-up may be closed. If maintainers request changes or
ask a question, you must respond within one week. PRs with no response for one
week after review feedback may be closed as abandoned.

Closed-as-abandoned PRs can still be useful. If you return later, open a fresh
PR or ask whether the old one should be reopened.

## Scope And Compatibility

Prefer config over code for provider and model quirks whenever possible.

Provider profiles should describe gateway behavior: base URLs, auth, headers,
endpoint paths, and gateway-specific corrections. Model-family catalogs should
describe model behavior that follows the model across providers: context
windows, modalities, reasoning behavior, search support, and tool quirks.

Avoid broad refactors in feature PRs. If a cleanup is needed first, split it
into a separate PR.

## Security

Do not report vulnerabilities in public issues or pull requests. Follow
[`SECURITY.md`](SECURITY.md) for private reporting.

Never include real credentials in examples, tests, logs, screenshots, fixtures,
or debug output.
