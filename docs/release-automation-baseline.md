# Release Automation Baseline

Recorded on 2026-08-30 before release automation implementation.

## Repository

- Repository: `jatmn/Codex-warp`
- Default branch: `main`
- Baseline commit: `023d4a2e70a069fdfa4053f7f5d724f42329d63e`
- Cargo version: `0.0.1`
- Visibility: public
- Merge mode: squash only; merge commits and rebase merges disabled
- Required status: `Source Checks`, strict/up-to-date
- Linear history, conversation resolution, admin enforcement: enabled
- Force pushes and branch deletion: disabled
- Default workflow token: read-only
- Workflow-token PR creation/approval: disabled

## Existing release state

- Git tags: none
- GitHub Releases: none
- Repository rulesets: none
- GitHub environments: none
- Release workflows: none

Existing workflows were CI, Mutants, Supply Chain, and Dependabot Updates.

## Pinned implementation inputs

The machine-readable authority is `tools/release-tooling.json`; downloaded dist
archive digests are in `tools/dist-tool-digests.sha256`.

- Release Please Action 5.0.0 at
  `45996ed1f6d02564a971a2fa1b5860e934307cf7`
- Embedded Release Please 17.6.0 at
  `712fcf01effd08d7b0e7b1fd3861f2cb388bc8d1`
- dist 0.32.0 at `6886366640dd4da83d33ba55cc04aa58423cbad2`
- Node.js 24.20.0 LTS
- Rust 1.98.0

The clean baseline passed `bash scripts/ci-preflight.sh`. The restricted runner
initially denied loopback socket binding; rerunning the identical command with
normal host test permissions passed all 930 tests and every remaining gate.

## Activation boundary

No production App, secret, environment, ruleset, branch, tag, release, or
repository variable was created during this baseline. Publication remains
disabled until the guarded sandbox and repository-setting phases in the plan.
