# Releases

Codex Warp has two release channels. Official releases are stable, versioned
builds such as `v0.1.0`. Nightlies are immutable prereleases named
`nightly-YYYYMMDD-SHA12` and are intended for testing.

The automation is intentionally disabled until the repository owner completes
the protected GitHub App, environment, and ruleset activation in the
[maintainer runbook](release-maintainer-runbook.md). A checked-in workflow is
not evidence that publication is enabled.

## Official Releases

Normal pull requests use Conventional Commit titles. Release Please maintains
one release pull request that updates `Cargo.toml`, `Cargo.lock`,
`.release-please-manifest.json`, and `CHANGELOG.md`. Merging that reviewed pull
request creates an immutable `vMAJOR.MINOR.PATCH` tag and a draft GitHub
Release. The tag starts the distribution workflow; the draft is published only
after every target, checksum, metadata file, and attestation verifies.

Official releases contain exactly eleven assets:

- four native archives for Linux x86-64, macOS Apple Silicon, macOS Intel, and
  Windows x86-64;
- one SHA-256 file per archive;
- `sha256.sum` for all four archives;
- cargo-dist's unmodified `dist-manifest.json`; and
- `codex-warp-release-metadata.json`, which binds the release to its source,
  toolchain, packaging contract, runner evidence, workflow, tag, and draft.

Archives include the executable, `codex-warp.toml`, the complete `configs/`
tree, `README.md`, `LICENSE`, `NOTICE`, and `CHANGELOG.md`. They do not include
credentials, logs, databases, or build directories.

## Nightly Releases

The nightly workflow runs at 03:17 in `America/Los_Angeles` when GitHub's
scheduler and the repository publication gate are enabled. It can also be run
manually in dry-run mode. A nightly identity is source-exact:

```text
tag:     nightly-20260830-0123456789ab
version: 0.1.0-nightly.20260830.0123456789ab
```

The date comes from the original workflow run timestamp. The suffix is the
first twelve characters of the full commit SHA. Nightly builds do not change
`Cargo.toml`; the workflow supplies the prerelease version at compile time.

Each published nightly is an immutable GitHub prerelease and never becomes the
repository's Latest release. After publication, the protected `nightly` branch
is created or fast-forwarded to the same source commit. A rerun for an already
verified source is a no-op or a branch-only repair; it never reuses or moves a
tag.

Nightly assets contain four archives, four per-archive checksum files,
`sha256.sum`, and `codex-warp-nightly-manifest.json`. Old nightlies are not
deleted automatically.

## Verify A Download

Download an archive, its matching `.sha256` file, and the channel manifest from
the GitHub Release. On Linux:

```bash
sha256sum -c codex-warp-x86_64-unknown-linux-gnu.tar.xz.sha256
gh attestation verify codex-warp-x86_64-unknown-linux-gnu.tar.xz \
  --repo jatmn/Codex-warp
```

Nightly archive names include the nightly tag before the target triple. macOS
can verify the recorded digest with `shasum -a 256 <archive>`. The project
manifest records the exact source SHA and expected digest for every archive.

Release binaries and archives are source-exact and traceable, not reproducible
build guarantees. Native runner images and native packaging tools can change;
their observed versions and digests are recorded so unexpected drift is
visible.

## Version Effects

With squash merges, the pull request title is the commit Release Please reads:

- `fix:` and `perf:` request a patch release;
- `feat:` requests a minor release;
- `type!:` or a `BREAKING CHANGE:` footer requests a breaking release; and
- documentation, tests, build, CI, refactors, and chores are normally hidden
  from release notes and do not independently request a release.

Before 1.0, this repository deliberately bumps minor for breaking changes.
Maintainers can use the documented `Release-As: X.Y.Z` footer for an exceptional
reviewed version override.
