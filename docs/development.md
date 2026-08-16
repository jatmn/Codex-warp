# Developer Build Guide

This guide is for building Codex Warp from source on Linux, macOS, and Windows.

Codex Warp is a Rust 2024 project. It uses `reqwest` with `rustls`, so normal
builds do not require system OpenSSL development packages. The rustls provider
builds AWS-LC from source, so source builds need CMake.

## Table Of Contents

- [Prerequisites](#prerequisites)
- [Linux](#linux)
- [macOS](#macos)
- [Windows](#windows)
- [Local Validation](#local-validation)
- [Continuous Integration](#continuous-integration)
- [Source Layout](#source-layout)
- [Testing Layout](#testing-layout)
- [Running From Source](#running-from-source)
- [Useful Environment Variables](#useful-environment-variables)
- [Release Artifacts](#release-artifacts)

## Prerequisites

- Rust toolchain with Cargo
- Git
- A shell for running commands

Recommended Rust install path:

```bash
rustup default stable
rustup update
```

Check your toolchain:

```bash
rustc --version
cargo --version
```

## Linux

Install common build tools.

Debian or Ubuntu:

```bash
sudo apt update
sudo apt install -y build-essential cmake pkg-config curl git
```

Fedora:

```bash
sudo dnf install -y gcc gcc-c++ make cmake pkgconf-pkg-config curl git
```

Arch:

```bash
sudo pacman -S --needed base-devel cmake pkgconf curl git
```

Build and test:

```bash
cargo build
cargo test
```

Release build:

```bash
cargo build --release
./target/release/codex-warp --help
```

## macOS

Install Xcode Command Line Tools and CMake:

```bash
xcode-select --install
brew install cmake
```

Install Rust with `rustup`, then build:

```bash
cargo build
cargo test
```

Release build:

```bash
cargo build --release
./target/release/codex-warp --help
```

Apple Silicon and Intel Macs both build with the default host target. For
cross-target builds, install the target explicitly:

```bash
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin
```

## Windows

Recommended setup:

1. Install Rust from [rustup.rs](https://rustup.rs/).
2. Install Visual Studio Build Tools with the `Desktop development with C++`
   workload.
3. Install CMake, either with the Visual Studio Build Tools CMake component or
   from [cmake.org](https://cmake.org/download/).
4. Open PowerShell or Windows Terminal.

Build and test:

```powershell
cargo build
cargo test
```

Release build:

```powershell
cargo build --release
.\target\release\codex-warp.exe --help
```

If Cargo cannot find a linker, reopen the terminal after installing Visual
Studio Build Tools, or run from a Developer PowerShell.

## Local Validation

Run these before committing code changes:

```bash
bash scripts/source-checks.sh
cargo test --locked
cargo build --locked
git diff --check
```

`scripts/source-checks.sh` runs rustfmt, `typos`, docs whitespace, docs
contraction capitalization (lowercase first-person contractions outside code
spans), `node --check` for the Web UI scripts, the chart harness, and crate-wide
Clippy (`cargo clippy --locked --all-targets --all-features -- -D warnings`). Those Clippy hits are
review findings. Install the spell checker with `cargo install typos-cli --locked`.

The chart harness covers `chart-math.js` policy (ticks, hover identity, keyboard
ownership, pointer reclaim only on hit, paint only with a measured CSS width,
live-region clear, canvas interactivity attrs, bar paint anchors) and
`footer-status.js` (analytics footer copy when chart-math is missing, boot
errors skipping that overlay). It is not a browser canvas stub of `app-main.js`.

For documentation-only changes:

```bash
SOURCE_CHECKS_CLIPPY=0 bash scripts/source-checks.sh
git diff --check
```

## Continuous Integration

GitHub Actions runs the source gate on pushes to `main` and on pull requests.
The CI job performs:

- `cargo update --workspace --locked` so `Cargo.lock` stays in sync with
  `Cargo.toml`
- `typos` spell check (`_typos.toml`)
- `scripts/source-checks.sh` (rustfmt, docs whitespace and contraction
  capitalization, Web UI JavaScript syntax and chart harness, crate-wide
  Clippy with `cargo clippy --locked --all-targets --all-features -- -D warnings`)
- `cargo test --locked`
- `cargo build --locked`
- CLI smoke checks for `codex-warp --version` and `codex-warp --help`
- `git diff --check`

Pull requests that touch Rust also run
`cargo mutants --no-shuffle -vV --in-diff git.diff -- --locked`
against the PR base SHA. Surviving mutants on changed lines are a test-quality
finding, not a request to add extra unrelated tests.

A separate supply-chain workflow runs `cargo-deny` (licenses, crate bans, and
crate sources) and `cargo-audit`. Advisory failures are non-blocking so a new
CVE does not freeze unrelated work. Do not add `_typos.toml`-style ignore
entries in `deny.toml` to hide a real license or git-source policy break.

## Source Layout

The crate is split by domain so small source changes do not all collide in one
file:

| File | Purpose |
| --- | --- |
| `src/main.rs` | Module map and `main()` entrypoint. |
| `src/server.rs` | CLI parsing, startup, routes, and top-level handlers. |
| `src/state.rs` | Shared request state and selected-provider structs. |
| `src/provider.rs` | Provider selection and provider display names. |
| `src/config.rs` | TOML config schema and defaults. |
| `src/config_loader.rs` | Config includes, TOML merging, and provider lookup. |
| `src/models.rs` | `/models` catalog fetching, sorting, and metadata shaping. |
| `src/upstream.rs` | Upstream request dispatch and response plumbing. |
| `src/structured_output.rs` | Chat Completions JSON Schema compatibility fallback. |
| `src/guardian_compat.rs` | Guardian auto-review prompt compatibility shim. |
| `src/namespace_helpers.rs` | Codex namespace-tool expansion for sub-agent helpers. |
| `src/response_codec.rs` | SSE, chat/responses conversion, and usage normalization. |
| `src/transform.rs` | Responses-to-chat request/tool/history conversion. |
| `src/transform_morph.rs` | Configured request morphs and dotted-path edits. |
| `src/tool_policy.rs` | Optional downstream tool-call approval policy. |
| `src/debug_log.rs` | Sanitized debug log events and fingerprints. |
| `src/process_log.rs` | In-memory process log buffer and tracing filter reload. |
| `src/http.rs` | Shared HTTP headers, endpoint URLs, and proxy errors. |
| `src/ids.rs` | Generated Responses item/call ids. |
| `src/version.rs` | Agent name and version reporting. |

## Testing Layout

Most unit tests live in sibling files named `src/<module>_tests.rs`, included
from the production module with `#[cfg(test)]` and `#[path = "..."]`. Keep new
tests near the module they exercise instead of adding a new root-level test
bundle. Test-quality rules live in [`AGENTS.md`](../AGENTS.md).

## Running From Source

Use `cargo run` during development:

```bash
export XIAOMI_TOKEN_PLAN_API_KEY="..."
cargo run -- --config configs/xiaomi-token-plan.toml
```

Or use a temporary provider destination:

```bash
cargo run -- --destination https://provider.example/v1
```

## Useful Environment Variables

- `RUST_LOG=codex_warp=debug`: enables debug logging for this crate when
  `debug.tracing_filter` is unset. Warp captures this value when tracing
  starts; changing `RUST_LOG` later in the same process does not change the
  live filter. The Web UI Logs tab can override it at runtime through
  `debug.tracing_filter`.
- Provider-specific API keys such as `XIAOMI_TOKEN_PLAN_API_KEY`.

Example:

```bash
RUST_LOG=codex_warp=debug cargo run -- --config configs/xiaomi-token-plan.toml
```

## Release Artifacts

Release binaries are produced under `target/release/`.

Typical artifact names:

- Linux/macOS: `target/release/codex-warp`
- Windows: `target\release\codex-warp.exe`

The runtime config files are not embedded in the binary. Keep `codex-warp.toml`
and any `configs/` profiles you want to use next to the working directory where
you launch the proxy, or pass explicit `--config` paths.
