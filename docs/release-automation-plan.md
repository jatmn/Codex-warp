# Codex Warp Release Automation Project Plan

Implementation copy created from the approved canonical plan with SHA-256
`a8a84c001463b3c9e8726123ff852ab308f01e5a666d05000cd71422564bb935`.
Implementation progress and any reviewed contract changes are recorded in this
version-controlled copy.

Status: implementation started on branch `release/release-automation`
Repository: `https://github.com/jatmn/Codex-warp`
Default branch: `main`
Prepared: 2026-08-30

Document custody: this is the local canonical planning copy until implementation
begins. Before Phase 0 work starts, place a second copy in a version-controlled
project/issue/docs location so loss of this machine cannot erase the plan.

## 1. Objective

Create a reliable two-channel release system for Codex Warp:

1. **Official channel** — human-approved Semantic Version releases prepared by
   Release Please, built for supported platforms, and published as GitHub
   Releases only after every required artifact succeeds.
2. **Nightly channel** — automated prereleases of the newest eligible `main`
   commit, plus a `nightly` branch that advances only after a successful nightly
   publication.

The system must keep `Cargo.toml` as the official version source of truth,
preserve the repository's existing CI and branch protections, use least-privilege
automation, package the runtime configuration files Codex Warp requires, and
provide safe retry and recovery paths.

## 2. Confirmed Product Decisions

- Use **Release Please** for official version calculation, release PRs,
  `Cargo.toml`/`Cargo.lock` updates, changelog maintenance, official tags, and
  draft GitHub Releases.
- Use **dist (formerly cargo-dist)** for official cross-platform archives,
  checksums, and release artifact orchestration.
- Use one explicit official handoff: Release Please creates a draft release and
  `v*` tag with a GitHub App token; that App-created tag triggers dist's generated
  tag workflow; dist runs with `create-release = false`, uploads to the existing
  draft, and publishes it only after complete verification.
- Keep official releases manually gated: merging the Release Please PR is the
  release decision.
- Use Conventional Commit semantics through squash-merged PR titles.
- Publish nightlies as GitHub prereleases with immutable, uniquely named tags.
- Maintain a `nightly` branch as a monotonic pointer created once or
  fast-forwarded only after nightly publication. A publish-before-branch
  transient is an explicitly detected failed transaction that must enter branch
  repair; the pointer is never moved backward to disguise it.
- Never push nightly version changes into `main` or `nightly`.
- Initially distribute archives, not shell/PowerShell installers, because
  Codex Warp requires `codex-warp.toml` and `configs/` at runtime and dist's
  simple installers retain only the executable.
- Do not publish to crates.io in this project.
- Do not enable GitHub release immutability until the complete draft → upload →
  publish path has passed a real official release.
- Define the initial reproducibility claim narrowly: releases are source-exact,
  traceable, and rebuildable from recorded inputs, with independently verifiable
  hashes and provenance. GitHub-hosted runner labels and their native toolsets are
  mutable, so initial delivery does not promise byte-for-byte identical archives
  from a later rebuild. Bit-for-bit reproducibility is a separately tested later
  enhancement, not an implied acceptance criterion.

## 3. Current Repository Baseline

The implementation must start by rechecking these observations against live
`main`:

- Package name: `codex-warp`.
- Current Cargo version: `0.0.1`.
- Version reporting currently uses `env!("CARGO_PKG_VERSION")` in
  `src/version.rs`.
- No Git tags or GitHub Releases currently exist.
- No release workflow currently exists.
- CI runs Source Checks on Linux and tests/builds on Windows.
- `main` requires the `Source Checks` status and linear history.
- The repository permits squash merging only, with the PR title becoming the
  squash commit title and the PR body becoming the commit body.
- Workflow permissions default to read-only.
- GitHub Actions currently cannot create or approve pull requests.
- Release binaries require external runtime assets: `codex-warp.toml` and the
  desired `configs/` profiles are not embedded.
- The repository includes `LICENSE` and `NOTICE`; both are release-contract
  inputs, and no `rust-toolchain.toml` pin exists yet.
- Repository policy requires third-party GitHub Actions to be pinned to full
  commit SHAs.

## 4. Target User Experience

### 4.1 Normal development

1. A contributor opens a PR with a Conventional Commit title such as
   `feat(webui): add provider health status`.
2. CI validates the title and existing source/test requirements.
3. The PR is squash-merged into `main`.
4. Release Please creates or refreshes one pending release PR.
5. The pending PR shows the proposed version, `Cargo.toml`/`Cargo.lock` changes,
   and the accumulated changelog.

### 4.2 Official release

1. A maintainer reviews the pending Release Please PR.
2. The maintainer verifies release prose, using Release Please's documented
   commit-override mechanism for corrections, and merges the PR.
3. Release Please creates the official `vX.Y.Z` tag and a draft GitHub Release.
4. The GitHub App-created `vX.Y.Z` tag triggers the generated dist workflow.
5. dist verifies that the matching draft exists and targets the same SHA, then
   builds every required target and runs archive smoke tests.
6. It generates SHA-256 checksums and GitHub build-provenance attestations.
7. It uploads all assets to the matching draft release.
8. Only after every required job succeeds does it publish the release.
9. GitHub marks it as the latest stable release.

If packaging fails, the release remains a draft and is not presented as an
official usable release.

### 4.3 Nightly release

1. A scheduled workflow selects an exact `main` SHA.
2. It exits successfully without publishing only if a complete verified
   prerelease already targets that SHA and `nightly` points to it; inconsistent
   partial state fails closed or enters the guarded branch-repair path.
3. It builds, packages, and smoke-tests the supported targets.
4. It retains attested publication intent, creates and verifies one immutable
   unique tag, and retains the tag-creation receipt before creating a draft.
5. It uploads/verifies the draft and publishes it as a prerelease.
6. It advances `nightly` to the exact source SHA only after publication.

If any step fails, the previous nightly release and branch pointer remain the
known-good nightly.

## 5. Version and Tag Policy

### 5.1 Official versions

- Follow Semantic Versioning.
- Tags use `vMAJOR.MINOR.PATCH`, for example `v0.1.0`.
- `Cargo.toml` is the official version source of truth.
- Release Please is the only normal automation allowed to bump the official
  version.
- Normal feature/fix PRs must not bump versions.
- Before `1.0.0`:
  - `fix` produces a patch bump.
  - `feat` produces a minor bump.
  - a breaking change produces a minor bump rather than jumping to `1.0.0`.
- After `1.0.0`, standard SemVer major/minor/patch behavior applies.
- An exact exceptional version can be requested with a documented
  `Release-As: X.Y.Z` commit footer; maintainers must not edit the manifest as a
  routine version-selection mechanism.

### 5.2 Nightly identities

- GitHub tag format:
  `nightly-YYYYMMDD-SHA12`, for example
  `nightly-20260830-a1b2c3d4e5f6`.
- The non-SemVer tag namespace prevents Release Please from mistaking nightly
  tags for official history.
- Release title format:
  `Nightly YYYY-MM-DD (SHA12)`.
- Nightly releases are always marked `prerelease=true` and `latest=false`.
- Nightly binary version format:
  `OFFICIAL_BASE-nightly.YYYYMMDD+SHA12`, for example
  `0.1.0-nightly.20260830+a1b2c3d4e5f6`.
- The nightly version is derived only from the source SHA, date, and Cargo base
  version. A retry for the same tag must report the same version; workflow run
  numbers and attempts must not enter artifact identity.
- Define the date as the calendar date in `America/Los_Angeles` derived from the
  original GitHub workflow run's immutable `created_at` timestamp. Query that
  timestamp by `github.run_id`; a rerun keeps the same run ID/date even after
  midnight. Never derive identity from the job's current clock.
- Official builds do not set a build-version override and report the exact
  Cargo version.
- Nightly tags are never moved or reused.
- The `nightly` branch is created once when absent and thereafter fast-forwarded
  only; it contains the same commit as `main`, not a generated nightly version
  commit.

## 6. Conventional Commit and Changelog Policy

### 6.1 Accepted PR title forms

Use this shape:

```text
type(optional-scope)!: concise description
```

Accepted types:

- `feat`
- `fix`
- `perf`
- `refactor`
- `docs`
- `test`
- `build`
- `ci`
- `chore`
- `revert`

Examples:

```text
feat(config): add reusable provider fragments
fix(webui): preserve inherited stream usage
perf(codec): reduce markup scanning allocations
feat!: change the provider selection contract
```

### 6.2 Version effects

- `feat` → minor.
- `fix` → patch.
- `perf` → patch unless deliberately configured otherwise.
- `revert` → patch. A user-visible rollback is release-worthy; if the pinned
  Release Please version does not treat `revert` this way under the selected
  changelog configuration, contributors must use `fix(revert): ...` and the
  accepted-type policy must be revised before automation is enabled.
- `!` or `BREAKING CHANGE:` → breaking-version policy.
- `docs`, `test`, `build`, `ci`, `chore`, and `refactor` appear only in the
  configured changelog sections and do not independently force a release unless
  explicitly configured.
- Dependabot titles such as `build(deps): ...` remain valid.
- Release Please's own `chore(main): release X.Y.Z` title remains valid.
- Treat these effects as policy assertions, not assumptions about Release
  Please. Before configuration is accepted, a pinned-version policy harness
  must verify every accepted type: `feat`, `fix`, `perf`, `revert`, `refactor`,
  `docs`, `test`, `build`, `ci`, and `chore`, plus scoped forms,
  `build(deps)`, both `!` and valid `BREAKING CHANGE:` forms, a malformed
  breaking footer, and `Release-As` histories. Record the expected bump,
  no-release result, and changelog section for every case.
- Implement that harness in `tools/release-please-policy/` as a small checked-in
  JavaScript adapter plus fixture data, `package.json`, and committed
  `package-lock.json`. Pin an explicit Node LTS patch version and install with
  `npm ci --ignore-scripts`; do not add runtime JavaScript dependencies to Codex
  Warp itself.
- Inspect the pinned `googleapis/release-please-action` commit and pin the exact
  `release-please` dependency version it executes, not merely a similarly named
  npm release. The lockfile's integrity hashes are part of the test contract.
- Construct isolated synthetic commit histories and invoke Release Please's own
  Conventional Commit parsing, version-selection, and changelog logic. Use only
  exports from the package's public `index` interface where they cover the test.
  If the exact pinned version requires a specific internal import for a missing
  public seam, name that import explicitly as a pinned compatibility dependency;
  the harness must fail on package upgrade until the import is reviewed and
  adapted. Do not reimplement SemVer bump rules in shell or push fixture branches
  to this repository or a disposable production-adjacent repository.
- Record a human-readable expectation table and machine-check the candidate
  version and changelog section for every fixture. Pinning or API failure is a
  hard test failure; there is no network-backed fallback repository.
- If the pinned tool cannot implement the stated policy, change the supported
  configuration or revise this policy explicitly before merging automation;
  never ship a known mismatch.

### 6.3 Enforcement

- Add PR-title validation to the existing required `Source Checks` job rather
  than introducing an unprotected, optional check.
- Validate `github.event.pull_request.title` only for pull-request events.
- Pass the untrusted PR title to shell through an environment variable or
  argument; never interpolate the title directly into a `run:` script.
- Put the regex and readable error output in a repository script with a small
  deterministic harness so CI behavior is locally testable.
- Document the convention in `CONTRIBUTING.md` or the existing contributor
  documentation and PR template.
- Keep changelog sections intentionally user-facing; hide low-value maintenance
  noise by default while retaining links in Git history.

## 7. Release Please Design

### 7.1 Configuration files

Add:

- `release-please-config.json`
- `.release-please-manifest.json`
- `CHANGELOG.md`

The configuration must include:

- an immutable, full-commit schema URL for editor validation plus a vendored copy
  of that same schema for deterministic, network-free CI validation;
- release type `rust`;
- root package path `.`;
- tags with `v` and without a redundant component prefix;
- pre-1.0 breaking-change behavior described above;
- explicit changelog sections;
- `draft: true`;
- `force-tag-creation: true` so the draft release has a real tag for the
  distribution workflow;
- a first-release bootstrap boundary;
- the initial manifest value matching the pre-automation Cargo version.

Conceptual configuration (exact supported keys must be checked against the
pinned Release Please version during implementation):

```json
{
  "$schema": "https://raw.githubusercontent.com/googleapis/release-please/<FULL_RELEASE_PLEASE_COMMIT>/schemas/config.json",
  "release-type": "rust",
  "include-component-in-tag": false,
  "include-v-in-tag": true,
  "bump-minor-pre-major": true,
  "draft": true,
  "force-tag-creation": true,
  "packages": {
    ".": {
      "component": "codex-warp"
    }
  }
}
```

`<FULL_RELEASE_PLEASE_COMMIT>` is a planning placeholder, not a value that may be
committed. During Phase 0, resolve the exact `googleapis/release-please` source
commit used by the dependency inside the pinned Action, vendor its
`schemas/config.json` as `tools/release-please-policy/config.schema.json`, record
the schema digest, and replace the placeholder with that full commit SHA. CI
validates against the vendored file and must not fetch the schema from `main` or
another mutable ref.

Initial manifest:

```json
{
  ".": "0.0.1"
}
```

### 7.2 First-release bootstrap

The first automated official release needs special handling because there is no
prior tag:

1. Record the live `main` SHA immediately before the release-automation setup
   PR and use it as the bootstrap boundary.
2. Seed `CHANGELOG.md` with a concise project introduction and format compatible
   with Release Please.
3. Put `Release-As: 0.1.0` in the setup PR's squash-commit body, or use the
   documented one-time bootstrap mechanism supported by the pinned version.
4. Inspect the generated first release PR carefully.
5. Curate the `0.1.0` notes through supported inputs. For a merged squash PR,
   edit that merged PR's body with a `BEGIN_COMMIT_OVERRIDE` /
   `END_COMMIT_OVERRIDE` block and rerun Release Please. Do not rely on direct
   edits to the bot branch surviving the next refresh. If the same overridden
   commit forces the bootstrap version, retain `Release-As: 0.1.0` inside the
   override block.
6. Put durable introductory prose in the changelog seed or documented release
   note template rather than an untracked manual edit.
7. Remove a temporary bootstrap override after the first official release if
   Release Please documentation says it is no longer needed.

### 7.3 Release Please workflow

Add `.github/workflows/release-please.yml` with:

- triggers on pushes to `main` and manual dispatch;
- a manual-dispatch guard requiring `github.ref == 'refs/heads/main'`; the
  token-minting job must also use the protected `release-automation`
  environment described below, so selecting another ref cannot expose the App
  private key or authorize mutation;
- a serialized concurrency group with `cancel-in-progress: false` and
  `queue: max`; pending Release Please runs remain queued rather than silently
  replacing one another;
- after the protected mutation job starts, a fresh fetch and detached checkout
  of live `main` before policy/config revalidation and private-key access; an
  event checkout may be stale after an environment or concurrency wait;
- a pinned Release Please Action SHA;
- a short-lived repository-scoped GitHub App installation token with only the
  four Release Please permissions from Section 8.2, including the documented
  `workflows: write` requirement for tag/draft creation across workflow-file
  changes;
- explicit job outputs for release-created state, version, tag, release URL,
  upload URL, and tagged SHA for summaries and recovery diagnostics;
- clear summary output stating whether it updated a release PR, created a
  draft, or had no releasable work.

The Release Please workflow does not build packages. Its App-created `v*` tag is
the sole normal trigger for the generated dist workflow. The dist workflow must
listen only for official SemVer tags, never `nightly-*`, and must use
`create-release = false`. Do not rely on a `release.published` event: the release
is supposed to remain a draft until dist succeeds.

The integration proof must verify the ordering of forced tag and draft creation.
At the dist host/finalize phase, retry draft lookup for a short bounded period,
then fail closed if the exact draft does not exist. If Release Please creates a
tag but fails before creating the draft, dist must not create or publish a
replacement release.

Treat that correct-tag/missing-draft state as an explicit Release Please
continuation, not an ordinary dist recovery. The pinned Release Please Action
treats an existing Git tag as the release and will not POST a GitHub draft for
it. The separately reviewed recovery is `scripts/create-missing-official-draft.sh`
in the Release Please workflow: after the App-token prior-release recheck, it
creates one unpublished official draft bound to that tag's peeled SHA and skips
the Release Please action so overlay commits after the tag cannot open a newer
version PR. Do not use `gh release create` for this state. After that draft
exists, rerun the failed dist run or use the guarded existing-draft recovery
workflow. Record the originating and continuation run IDs, exact release PR,
tag, peeled SHA, and resulting release ID. Never improvise a release in the
production repository.

Add a required release-readiness mode to the existing protected `Source Checks`
job. Store the trusted classifier inputs in checked-in
`tools/release-automation-policy.json`, validate it offline against
`tools/release-automation-policy.schema.json`, and change it only through normal
protected review. Classify a pull request as the Release Please PR only when all
of these recorded facts match: the base repository is `jatmn/Codex-warp`, the
base branch is `main`, the head repository is the same repository, the head ref
is the exact configured Release Please branch for `main`, and the pull request
creator's stable bot account ID, login, and `type=Bot` match the installed
release App bot identity recorded after installation. Use the pull request creator from the API,
not `github.actor`, because a later synchronize/rerun actor is not authoritative.
The changed-file set must also match the checked-in Release Please output
contract: it contains the required version/changelog/manifest files and no file
outside the explicitly allowed release-PR set. A label may corroborate this
classification but is never identity evidence.

If every identity predicate matches, the job must use the read-only GitHub API
and fail when:

- any official `v*` GitHub Release is still a draft;
- an earlier official SemVer tag has no complete published release; or
- an official finalizer/recovery concurrency group is active for an earlier tag.

A new Release Please PR still cannot merge in the correct-tag/missing-draft
state. The Release Please workflow itself may continue that newest tag: it
allows the latest official SemVer tag to have zero release objects, then
rechecks with the App token because GitHub hides unpublished drafts from
contents:read `GITHUB_TOKEN`. `scripts/create-missing-official-draft.sh` then
creates one unpublished draft for the existing unmoved tag and skips the
pinned Action so that continuation cannot propose a newer version.

Ordinary contributor PRs do not need this release-state check. A PR that matches
the configured release branch or recorded App identity but fails any other
Release Please identity/output predicate is automation-shaped ambiguous state;
it must fail closed with a diagnostic rather than fall through as ordinary.
Fixtures must cover the genuine App PR plus branch-only, author-only, fork-head,
wrong-base, changed-event-actor, and unexpected-file spoof/misconfiguration
cases. The Release Please workflow must also refuse to create/finalize a newer
official release while an earlier official draft is outstanding. This is defense
in depth: the forced tag lets Release Please recognize a version before dist has
actually published its release, so tag existence alone is not proof that the previous
official release completed.

## 8. Bot Identity, Settings, and Permissions

### 8.1 GitHub App

Create a repository-scoped GitHub App for release automation rather than using
a maintainer's long-lived personal token.

Suggested name: `codex-warp-release-bot`.

Required repository permissions:

- Metadata: read.
- Contents: read and write.
- Pull requests: read and write.
- Issues: read and write, because Release Please manages labels.
- Workflows: read and write. Request it only for the bounded Release Please
  tag/draft transaction and guarded final mutation jobs, because GitHub can
  require it when creating or updating refs/releases whose target differs from
  the default branch under `.github/workflows/`.
- Actions: read only if token generation or Release Please API behavior requires
  it during the implementation proof.

No organization-wide installation is required. Install the production App only
on `jatmn/Codex-warp`; destructive sandbox proofs use the separate sandbox-only
App defined in Phase 0.

Store:

- App client ID as an Actions variable named `RELEASE_APP_CLIENT_ID`.
- Private key as an environment secret named `RELEASE_APP_PRIVATE_KEY` in a
  `release-automation` environment, not as a repository-wide secret. Initially
  configure that environment's deployment-ref rules for `main` only. Add
  official `v*` tag refs only after the release App is installed and the separate
  `v*` creation and immutability rulesets have been created, verified to allow
  new tag creation only by that App, and verified to reject App and routine
  rewrite/deletion. Never create a window in
  which an unprotected `v*` ref can enter the secret-bearing environment. Do not
  allow arbitrary branches or tags. The Release Please, official mutation,
  nightly mutation, and recovery jobs that mint App tokens must declare this
  environment. An environment is a job-level boundary: read-only preparation
  and verification jobs must not declare it. A dependent mutation job may
  declare it only after its `needs` gates have succeeded, and it must revalidate
  live state before the private key is first passed to the token-minting action.
  Do not add an environment reviewer that would create a second normal-release
  approval: the official human gate remains the release-PR merge, while nightly
  publication remains gated by the schedule variable or explicit non-dry-run
  input.

Generate installation tokens inside the workflow with the official GitHub
App-token action pinned to a full commit SHA and scope every token explicitly to
the current repository. The Release Please job requests only `contents: write`,
`pull-requests: write`, `issues: write`, and `workflows: write`. The last
permission is required because GitHub can reject tag/draft creation when the
release target differs from a moving default branch under `.github/workflows/`;
Release Please concurrency does not serialize unrelated merges to `main`.
Protected `main`, the protected official-tag namespace, and the bounded
Release Please job contain that permission. Never print the private key or
token, and let the action revoke it at the end of the job.

Guarded final mutation jobs—normal official publication, official recovery, or
the nightly release/ref transaction—mint a separate token narrowed to
`contents: write` and `workflows: write`, after all applicable ref/SHA/asset
guards pass. Official release updates can require this when `main` changes
workflow files while an older tagged draft is building. Nightly release updates
and advancing `refs/heads/nightly` can require it when the selected commit
crosses a `.github/workflows/` change. This avoids a maintainer PAT and contains
the elevated permission to the exact bounded jobs for which GitHub may require
it.

### 8.2 Workflow permissions

- Keep repository default workflow permissions read-only.
- Treat `GITHUB_TOKEN` and the release App token as separate credentials.
- Release Please job `GITHUB_TOKEN`: `contents: read` or `{}` if checkout is not
  needed. Workflow `permissions:` does not restrict the separately generated App
  token.
- Release Please App token: explicitly narrowed to `contents: write`,
  `pull-requests: write`, `issues: write`, and `workflows: write` when it is
  generated. This token alone owns release-PR updates plus official tag/draft
  creation. Final mutation tokens use the two-permission scopes stated below
  instead.
- Build jobs: `contents: read`.
- Release-readiness mode in Source Checks: `contents: read`, `actions: read`, and
  `pull-requests: read` only; the last permission is limited to authenticating
  the PR creator/head/base and paginating its changed-file inventory. It must not
  receive an App token or write permission.
- Attestation jobs: `contents: read`, `id-token: write`,
  `attestations: write`.
- Normal official build and collection/local-verification jobs remain read-only
  and do not declare `release-automation`. Their dependent mutation job
  revalidates live state, then generates the App token narrowed to
  `contents: write` and `workflows: write` and uses it for upload/undraft; do not
  use `GITHUB_TOKEN` to publish the official release.
- Official recovery build/verification jobs: read-only. Only the dedicated
  `resume-upload` artifact-retrieval job additionally receives `actions: read`;
  it never receives an App token or contents write. `publish-verified-draft`
  `load-remote` also declares `release-automation` and mints the bounded App
  token because GitHub hides unpublished draft assets from contents:read
  `GITHUB_TOKEN`; that job still never uploads, deletes, or undrafts. All other
  recovery operations omit `actions: read`. The final existing-draft
  mutation/publish step uses the separately generated App token narrowed to
  `contents: write` and `workflows: write`; do not use `GITHUB_TOKEN` for this
  update. Only the `replace-unpublished-assets` operation may additionally give
  its mutation job `id-token: write` and `attestations: write`, solely to retain
  and attest the exact pre-delete intent and post-delete receipt required by
  Section 11.1. Other official recovery operations do not receive those
  attestation permissions unless they attest rebuilt outputs.
- Nightly build and collection `GITHUB_TOKEN`: `contents: read` except for the
  separate artifact-attestation permissions listed above. After all
  builds, smoke tests, current-main checks, and release-state checks pass, the
  dependent environment-bearing mutation job receives `contents: read`,
  `actions: read`, `id-token: write`, and `attestations: write` only so it can
  retain, retrieve-check, and attest the pre-tag intent and post-tag receipt
  required by Section 10.6. It revalidates live state and then generates one
  repository-scoped App token narrowed to `contents: write` and
  `workflows: write` and uses it for tag/draft creation, upload, publication,
  and the guarded `nightly` ref update. Do not use `GITHUB_TOKEN` for nightly
  release or ref mutation.
- Nightly prepare job: `contents: read` and `actions: read` so it can retrieve
  the current run's immutable `created_at` timestamp.
- The nightly recovery plan/evidence-reader job receives `contents: read` and
  `actions: read`, because cross-run inspection and download of retained evidence
  must use the authenticated Actions APIs rather than silently depending on this
  repository remaining public. The operation-specific replacement mutation job
  may also receive `id-token: write` and `attestations: write` solely to attest
  the pre-delete intent and post-delete receipt described in Section 11.2. Other
  recovery operations do not receive those attestation permissions unless they
  attest build outputs. No recovery `GITHUB_TOKEN` receives contents write.
- Do not add a crates.io token.

### 8.3 Branch and tag controls

- Preserve the existing `main` protection and required `Source Checks` status.
- Ensure App-created Release Please PRs trigger normal CI.
- Keep the repository setting that allows `GITHUB_TOKEN` to create/approve PRs
  disabled; the dedicated App owns automated PRs.
- Do not permit the App to bypass `main` reviews or required checks.
- The bot may create/update its Release Please branch and create official tags.
- GitHub grants bypass to an actor for an entire ruleset, not for selected rules
  inside it, while rules from multiple matching rulesets aggregate. Therefore do
  not combine an App-bypassed creation/update rule with the deletion/rewrite rules
  that must also constrain the App.
- Before `release-automation` permits an official `v*` deployment ref, create two
  active tag rulesets targeting GitHub's `v*` fnmatch namespace. Ruleset fnmatch
  is deliberately broader than SemVer; the workflow's anchored runtime regex
  remains the exact stable-tag syntax gate:
    1. a creation ruleset with `Restrict creations`, bypassed only by the release
       App and documented human break-glass actor; and
    2. an immutability ruleset with `Restrict updates` and `Restrict deletions`,
       bypassed only by the documented human break-glass actor, never by the App.
  Configure and verify both after installing the App but before adding the
  environment's `v*` ref rule; the order is an activation invariant, not merely
  a first-release checklist item.
- Before the first nightly publication, create the same two-ruleset pattern for
  `nightly-*`: the App may bypass only `Restrict creations`; a separate update/
  deletion ruleset has no App bypass. No routine actor may move or delete an
  existing nightly tag. Any human deletion bypass is break-glass recovery only
  and must refuse a published nightly.
- Protect `nightly` with two active branch rulesets. A writer ruleset restricts
  creation/updates and lets only the release App plus human break-glass bypass
  it. A separate integrity ruleset blocks force pushes and deletion without an
  App bypass. The App may create the branch once when absent or fast-forward it
  to a commit reachable from `main`; it cannot force-push or delete it.
- In the parity-configured sandbox, test the resulting aggregate policy with a
  real narrowed sandbox App token: it can create fresh matching tags and create/
  fast-forward `nightly`, but attempts to update/delete existing tags, delete
  `nightly`, or make a non-fast-forward branch update are rejected. A sandbox
  write collaborator must also be unable to create or mutate those protected
  refs. Record cleanup receipts under the sandbox policy.
- In production, do not create any ruleset-probe `v*` or `nightly-*` tag and do
  not make a destructive call whose success would damage a legitimate ref.
  Before activation, retrieve the live rulesets/environment policy through the
  read-only API, compare their IDs, targets, enforcement state, rules, and bypass
  actors with the sandbox-proven configuration, and do not invoke a production
  Git-ref write API merely to test the policy. The first normal nightly
  and first normal official release are the production positive creation tests;
  immediately afterward, re-read the unchanged ref and ruleset configuration.
  Configuration parity plus sandbox mutation evidence—not a throwaway production
  tag—is the production immutability proof.

## 9. Official Artifact Design

### 9.1 Initial supported matrix

Required targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-pc-windows-msvc`

Deferred until independently validated:

- `aarch64-unknown-linux-gnu`
- musl/static Linux builds
- Windows ARM64
- MSI, Homebrew, npm, shell, and PowerShell installers

Use explicit runner images that support the target at implementation time;
avoid mutable `*-latest` labels where a fixed supported image exists. Confirm
CMake and required native build tools in each job rather than assuming runner
contents indefinitely. Record the exact runner image version and resolved native
tool versions in release evidence. A fixed OS label such as `ubuntu-24.04` still
receives image updates and is not a byte-reproducible build-environment pin.

#### Rebuildability scope

Initial delivery guarantees that each artifact is bound to immutable source,
the exact Rust pin, lockfile and packaging-contract digests, the observed runner
image/native tools, checksums, and GitHub provenance. A maintainer can rebuild
the same source and explain all selected inputs, but later bytes may differ when
GitHub rotates a hosted image or native SDK. Documentation and acceptance tests
must use **source-exact**, **traceable**, and **rebuildable from recorded inputs**
for this guarantee; they must not claim bit-for-bit reproducibility.

Byte-for-byte reproducibility requires a separate contract covering normalized
archive ordering, timestamps, modes, owners, locale/time zone, deterministic
compression, pinned base images/native dependencies where feasible, and two
independent clean builds with identical hashes on every supported target. That
work is explicitly deferred to Section 19.

#### Rust toolchain pin

Add `rust-toolchain.toml` with an exact stable Rust patch version and the minimal
profile. List only the components actually required by repository validation,
such as `rustfmt` and `clippy`; target installation remains explicit in the
platform jobs. This file at the immutable source commit is the single toolchain
authority for local source checks, generated dist CI, nightly builds, and both
recovery workflows. No release workflow may select `stable`, use an unpinned
runner default, or override it with a second dist-specific toolchain setting.

Each manifest records the resolved `rustc -Vv` and Cargo versions and verifies
that they satisfy the source commit's exact pin. Historical recovery installs
the pin from the tagged/selected source, not today's `main`. A toolchain update
is a normal reviewed dependency change: update the pin, regenerate/test the
official workflow, run the complete four-target dry-run proof, and record the
old/new versions. If an old pinned toolchain becomes unavailable, recovery must
use the explicit tag-specific recipe mechanism rather than silently substituting
a newer compiler.

### 9.2 Archive names

Official examples:

```text
codex-warp-x86_64-unknown-linux-gnu.tar.xz
codex-warp-aarch64-apple-darwin.tar.xz
codex-warp-x86_64-apple-darwin.tar.xz
codex-warp-x86_64-pc-windows-msvc.zip
```

These are dist's native archive names. The immutable release tag and download
URL carry the official version. Do not add the version to official filenames
with a post-build rename: dist's manifest, checksum names, tar root, download
table, and attestation subjects must remain mutually consistent.

Nightly examples:

```text
codex-warp-nightly-20260830-a1b2c3d4e5f6-x86_64-unknown-linux-gnu.tar.xz
codex-warp-nightly-20260830-a1b2c3d4e5f6-x86_64-pc-windows-msvc.zip
```

### 9.3 Archive contents

Use dist's native, documented archive layout:

- Unix tar archives contain one predictable top-level directory.
- Windows ZIP archives contain the files directly at the ZIP root.

The logical contents in either layout are:

```text
tar:
  <archive-basename>/
    codex-warp
    codex-warp.toml
    configs/
    README.md
    LICENSE
    NOTICE
    CHANGELOG.md

zip root:
  codex-warp.exe
  codex-warp.toml
  configs/
  README.md
  LICENSE
  NOTICE
  CHANGELOG.md
```

For example, the official Linux tar root is
`codex-warp-x86_64-unknown-linux-gnu/`. A nightly tar root is likewise the
nightly archive filename without `.tar.xz`. The contract tests must derive the
expected tar root from the archive basename rather than constructing a separate
versioned name.

Include all provider, model-family, and tool-policy configuration descendants.
`NOTICE` is mandatory in every official and nightly archive: it carries the
project's copyright, non-affiliation, and trademark attribution, and the
repository's Apache-derived distribution terms expressly account for NOTICE
material. The contract tests must compare its digest with the selected source
commit. Do not flatten `configs/`. Do not include API keys, local databases,
debug logs, build directories, or developer-specific config.

### 9.4 dist configuration

Add `dist-workspace.toml` (preferred over embedding a large block in
`Cargo.toml`) and configure:

- the pinned dist version;
- GitHub CI support;
- the four initial targets;
- `.tar.xz` for Unix and `.zip` for Windows;
- SHA-256 checksums;
- dist's unified `sha256.sum` checksum index in addition to per-archive
  `.sha256` files;
- GitHub artifact attestations;
- `auto-includes` for README/license/changelog;
- explicit `include` entries for `NOTICE`, `codex-warp.toml`, and `configs/`;
- no installers;
- no crates.io publishing;
- `source-tarball = false`, because GitHub already supplies source archives and
  the release asset contract is for runnable binary bundles;
- final steady-state `pr-run-mode = "plan"`; Phase 2 temporarily exercises the
  same generated workflow with `pr-run-mode = "upload"` and returns it to
  `plan` only after all four official target archives pass;
- `create-release = false`;
- Bring Your Own GitHub Release behavior so dist uploads to the draft created by
  Release Please and publishes only after all artifacts succeed;
- preserve dist's final `dist-manifest.json` byte-for-byte after its pinned
  merge/preparation step and validate it against the schema emitted by that exact
  dist version; Codex Warp provenance fields do not get injected into dist's
  schema-owned document; and
- full-SHA pins for generated third-party Actions, consistent with repository
  policy.

Generate the dist workflow rather than hand-maintaining its jobs. The pinned dist
template does not currently expose a tag-workflow concurrency setting, so use one
narrow, deterministic generation overlay:

1. Record the exact dist version and immutable SHA-256 digest for each
   platform-specific dist archive used by CI in
   `tools/dist-tool-digests.sha256`. Do not trust a versioned URL alone.
2. `scripts/generate-dist-workflow.sh` runs the pinned `dist generate`.
3. It invokes `scripts/patch-dist-workflow.sh`, which parses the generated YAML,
   verifies the expected template anchors, and performs only these reviewed
   transformations:
   - replace dist's broad version-like push-tag glob with the official-only
     stable-tag glob `v[0-9]+.[0-9]+.[0-9]+`, with no trailing wildcard. The plan
     job supports read-only pull-request planning and exact official tag pushes.
     Build jobs may run only for an exact tag push or, while Phase 2 temporarily
     selects `pr-run-mode = "upload"`, a same-repository pull request whose plan
     authorizes upload; fork PRs cannot enter the upload proof. On the tag path,
     runtime-match `^v[0-9]+\.[0-9]+\.[0-9]+$` before any tool download or build.
     Every host/finalizer job requires the exact tag-push condition, and the final
     mutation job must enforce it with a job-level `if` before its
     `release-automation` environment is evaluated. Thus PR planning/upload can
     build read-only artifacts while every hosting, environment, App-token, ref,
     and release-mutation job remains structurally unreachable;
   - insert a top-level tag-specific concurrency block whose group resolves to
     `official-release-<tag>`, with `cancel-in-progress: false` and `queue: max`;
   - replace the generated root `contents: write` permission with read-only
     defaults and explicit job permissions: plan/global/collection jobs receive
     `contents: read`, build jobs receive `contents: read` plus only their
     attestation permissions, and the host's `GITHUB_TOKEN` is read-only or
     empty;
   - replace every generated remote dist installer execution with
     `scripts/install-pinned-dist.sh`;
   - split the generated official host/finalizer into a read-only collection and
     local-verification job, a dependent non-environment metadata-attestation job,
     and a dependent mutation job. The collection job generates and schema-
     validates `codex-warp-release-metadata.json` from the final unmodified dist
     manifest and verified per-target evidence. The attestation job redownloads,
     revalidates, and attests that sidecar as a separate subject with only
     `contents: read`, `id-token: write`, and `attestations: write`. Only the
     mutation job declares the protected `release-automation` environment. It
     downloads the already-verified workflow artifacts, revalidates the exact
     live tag/draft state, then passes the private key to the pinned App-token
     action and requests only `contents: write` and `workflows: write`; bind every
     release mutation to that short-lived token instead of `GITHUB_TOKEN`. GitHub
     hides unpublished releases and their assets from contents:read `GITHUB_TOKEN`,
     so draft lookup, unpublished asset download, and pre-undraft remote checksum
     verification must use that App token; and
   - split the generated remote hosting transaction into distinct workflow
     steps: prepare the final dist manifest, upload assets to the existing draft,
     run the remote release-contract/SHA/checksum verification, and only then
     undraft the exact release ID. At pinned dist v0.32, the combined
     `dist host --steps=upload --steps=release` invocation is local manifest
     preparation: GitHub hosting is implemented by generated CI commands after
     it. The overlay may retain that one non-credentialed invocation, but must
     split the actual credential-bearing upload and release-edit commands. If a
     future dist version makes `dist host` mutate GitHub, regeneration must fail
     until the invocation is separated or replaced. No credential-bearing shell
     command or workflow step may both upload and undraft, so a verification
     failure necessarily leaves the release as a draft.
4. `install-pinned-dist.sh` selects the expected platform archive, downloads the
   exact version without executing a remote installer script, verifies it
   against the repository-recorded digest, and only then extracts/executes dist.
   The overlay check must fail if any unverified dist installer command remains.
5. Configure `allow-dirty = ["ci"]` in `dist-workspace.toml`, the documented dist
   escape hatch for an intentionally customized generated CI file. This opt-out
   applies only to dist's raw CI freshness check.
6. Replace raw `dist generate --check` with
   `scripts/check-dist-workflow.sh`: regenerate in an isolated temporary copy,
   apply the same overlay, and byte-compare the result with the checked-in
   workflow. Test that the patch is idempotent and fails if its anchors drift.

No one hand-edits the generated workflow. The recovery workflow must resolve the
identical concurrency group from its validated tag input. The custom generation
check must assert that the lock, exact stable-tag filter and runtime regex,
read-only `GITHUB_TOKEN` permissions, verified dist install, finalizer-only App
credential, and upload → verify → undraft separation remain present. It must
fail if the broad generated tag glob, a root/job `GITHUB_TOKEN` contents-write
grant, or any combined credential-bearing upload-and-release command reappears.
The check must distinguish the pinned non-mutating manifest-preparation
invocation from the subsequent GitHub API/CLI mutations. Normal finalization and
recovery may never mutate the same draft concurrently.

With four targets, no installers, and `source-tarball = false`, the intended
automation-attached official asset set is exactly eleven files: four binary
archives, four per-archive `.sha256` files, dist's unified `sha256.sum`, dist's
unmodified `dist-manifest.json`, and one separately schema-owned
`codex-warp-release-metadata.json`. GitHub's automatic source-code links and
GitHub-hosted attestations are not counted as attached release assets.

The project metadata sidecar—not `dist-manifest.json`—has a required schema-owned
`mode` discriminator with exactly two values:

- `official` binds a real official tag, recursively peeled full source SHA, Cargo
  version, and release identity. It requires `publishable=true` and requires the
  dist manifest's `announcement_tag_is_implicit` to be false. The tag ref must
  exist and peel to the recorded source SHA, and this is the only mode any
  finalizer/recovery may attach to or publish from a GitHub Release.
- `pr-upload-proof` is non-publishable evidence used only by the Phase 2
  same-repository PR run. It requires
  `announcement_tag_is_implicit=true`, `publishable=false`, and no claimed
  official tag, peeled tag SHA, or release ID. Instead it binds the repository,
  PR number, base SHA, PR head SHA, exact checked-out/build source SHA, merge SHA
  when applicable, workflow run/attempt, and final dist-manifest digest. An
  implicit announcement tag may be recorded only as dist diagnostic data and
  must never be promoted to official identity.

Both modes bind the exact source `rust-toolchain.toml` digest, resolved per-target
`rustc -Vv` and Cargo versions, `Cargo.lock` digest, runner image/native-tool
observations, release-contract digest, the digest of the final dist manifest,
and the target/archive/checksum mapping copied from and checked against that
manifest. The sidecar names its schema version and its own expected filename;
its own byte digest is supplied by its GitHub attestation and, for an official
release, the release-asset API rather than recursively embedded in itself. Any
disagreement between the sidecar, dist manifest, event/source identity, or local
files is a hard failure. The release-contract helper and finalizer must reject a
`pr-upload-proof` sidecar on every tag, upload, recovery, and publication path.

The checked-in release-contract helper exposes two explicit validation profiles,
`official-publication` and `pr-upload-proof`; callers must select the profile from
the already-validated workflow event/path, never from untrusted sidecar content.
Both profiles enforce the same eleven-file shape and cross-document artifact
mapping. The former requires official sidecar mode and a real tag/ref/release
identity; the latter requires proof mode and absence of publication identity.
No successful proof-profile result may be passed to an upload/finalizer step.

Do not duplicate the literal asset count across workflow code. At the pinned dist
version, generate the authoritative dist-owned inventory with
`dist manifest --artifacts=local --no-local-paths` and reconcile it with the
final merged, schema-valid dist manifest. The checked-in release-contract helper
then adds exactly the one named project metadata sidecar and validates the union
against the higher-level contract: four unique target archives, a checksum for
every archive, one unified checksum index, exactly one unmodified dist manifest,
exactly one project metadata sidecar, no installer, no source tarball, and no
unexpected public artifact.
The unified `sha256.sum` is required and must contain exactly the four archive
entries; it is not an unexpected artifact.
If a dist upgrade changes the generated inventory, the custom dist-workflow check
or release-contract test must fail with a readable contract-drift error until the
change is reviewed. Nightly derives the shared target/archive/checksum portion
from the same checked-in contract helper, but retains its own explicit one-
manifest nightly contract rather than inheriting the official sidecar count.

### 9.5 Official publication transaction

For every official validation, resolve identity from Git data rather than from
the release object's `target_commitish` field:

- the project metadata sidecar must have `mode=official` and `publishable=true`,
  and the dist manifest must have `announcement_tag_is_implicit=false`;
- the release `tag_name` must equal the expected tag;
- resolve the tag ref and recursively peel an annotated tag, if present, to one
  commit;
- that commit must equal the expected full release SHA;
- `Cargo.toml` at that commit must contain the tag's version; and
- treat `target_commitish` as informational only, because GitHub ignores it when
  the tag already exists.

1. Release Please creates a tag and draft release using the release App token.
2. The App-created official tag triggers the generated dist `push.tags`
   workflow; events created by `GITHUB_TOKEN` are not part of this handoff.
3. dist runs with `create-release = false`, finds the exact draft, and validates
   that release `tag_name`, peeled tag commit, expected full SHA, and
   `Cargo.toml` version agree.
4. Each target builds from the exact tagged SHA with `--locked` and the exact
   `rust-toolchain.toml` pin at that SHA; the resolved compiler/Cargo versions
   must match the project metadata sidecar, whose target/archive mapping must in
   turn match the corresponding entries in the unmodified dist manifest.
5. Each target runs binary and archive smoke tests.
6. Build outputs are passed through GitHub workflow artifacts, not public
   partial releases.
7. A read-only collection job collects every required output, creates both final
   metadata documents, and completes all local verification without declaring
   `release-automation`. A dependent non-environment attestation job re-downloads
   and verifies the sidecar, attests it with only the dedicated attestation
   permissions, and exposes its verified subject digest.
8. A separate mutation job depends on every build, collection, and sidecar-
   attestation gate and declares the protected `release-automation` environment.
   Once its protection
   rules pass and the job starts, it rereads and verifies the live tag, peeled
   SHA, exact draft release ID, and remote inventory. Only after that in-job
   revalidation does it pass the private key to the App-token action and mint an
   official-finalizer token narrowed to `contents: write` and
   `workflows: write`.
9. The mutation job uploads archives, `.sha256` files, `sha256.sum`, the
   unmodified dist manifest, and the attested project metadata sidecar to the
   existing draft, then rereads and verifies the exact eleven-file remote
   inventory, peeled tag commit, release `tag_name`, Cargo version, cross-manifest
   relationships, checksums, sidecar digest, and attestations.
   Build-provenance attestations are stored by GitHub's attestation service and
   verified with `gh attestation verify`. Official verification must constrain
   the certificate identity to the reviewed `Release` or `Release Recovery`
   signer workflow, expected tag/main source ref, source/control digest, and
   GitHub-hosted runner; repository scope alone is insufficient because other
   repository workflows may also mint attestations. Initial delivery does not
   upload an attestation bundle as a twelfth release asset.
10. In a separate command and workflow step, it undrafts that exact release ID
    through dist's generated hosting logic and lets the App token revoke at job
    completion. No upload command or shell step may also perform the undraft.

The finalizer must fail closed if any expected target, checksum, either metadata
document, attestation, or SHA relationship is missing or contradictory.

## 10. Nightly Workflow Design

Add `.github/workflows/nightly.yml`.

### 10.1 Triggers

- Daily schedule at a non-zero minute, for example 03:17
  `America/Los_Angeles`.
- `workflow_dispatch` for testing, retry, and on-demand nightlies.
- Manual dispatch must use `refs/heads/main`; a first prepare step verifies that
  ref, and the token-minting finalizer uses the `release-automation` environment
  whose deployment-ref policy independently rejects any other ref.
- Boolean manual input `dry_run`, default `true`. Keep the safe manual default
  permanently; a maintainer must explicitly choose publication.
- Optional manual string input `source_sha`, default empty. Empty selects the
  immutable event SHA from `main`. A nonempty value must be a full 40-character
  commit SHA reachable from `main` and may only inspect or recover already
  existing nightly state for that SHA; it must never create a fresh nightly for
  an arbitrary historical commit.
- Repository variable `NIGHTLY_PUBLISH_ENABLED`, initially `false`. Scheduled
  publication is permitted only when Phase 4 deliberately changes it to `true`.
- A workflow-level `nightly-release` concurrency group with
  `cancel-in-progress: false` and `queue: max`. Do not let a newer dispatch
  replace a pending run or cancel a run that may already be publishing.

Do not run nightly on every push; normal CI already serves that purpose. Compute
one `publish` output in the prepare job and use it to guard every mutation:

```text
publish =
  (event == schedule && vars.NIGHTLY_PUBLISH_ENABLED == 'true')
  ||
  (event == workflow_dispatch && inputs.dry_run == false)
```

A scheduled event with publication disabled still performs a dry run and writes
a clear summary. Never interpret a missing scheduled-event input as permission
to publish. Normalize the prepare job's `publish` output to the lowercase string
`true` or `false`. Every downstream mutation job and step must use an exact
`needs.prepare.outputs.publish == 'true'` comparison or an explicit
`fromJSON(...)` conversion; a bare nonempty output such as the string `false` is
not an authorization guard.

This repository is public, so GitHub may automatically disable scheduled
workflows after 60 days without repository activity. Document this platform
limit and the exact `gh workflow enable nightly.yml`/web-UI re-enable procedure.
Before scheduled publication is enabled, configure a read-only liveness monitor
that runs outside this repository's GitHub Actions. At least every 12 hours it
must query the public Actions runs endpoint twice: once with
`event=schedule&per_page=1` for the newest scheduled run regardless of state, and
once with `event=schedule&status=completed&per_page=1` for the newest completed
scheduled run. Cadence is measured only from the newest scheduled run's
`created_at`; never use `updated_at`, completion time, or `run_started_at` to
reset the 36-hour clock. Alert the named maintainer destination when no scheduled
run was created within 36 hours, when the newest completed scheduled run did not
succeed, or when the newest run remains queued/in-progress beyond a documented
six-hour stuck-run threshold. A late completion cannot clear a cadence alert
whose `created_at` is already stale. Record the chosen service, owner, notification
destination, endpoint, credential scope/rotation if authentication is used, and a
tested synthetic-alert procedure in the runbook. The alert first directs the
maintainer to check whether the workflow is enabled, inspect the latest run, and
use a guarded manual dispatch when appropriate. A monitor that runs inside the
same repository is not sufficient evidence of liveness because it can be disabled
by the same inactivity rule. If no external monitor is maintained, describe the
created-at cadence, failed-completion, and stuck-run checks only as manual
operational procedures and do not claim automated nightly liveness.

### 10.2 Eligibility and idempotency

The prepare job must:

1. For manual dispatch, verify `github.ref == 'refs/heads/main'`. Record the
   protected workflow SHA and the immutable default-branch event SHA.
2. Select the source SHA. Use the event SHA when `source_sha` is empty. Otherwise
   require a full 40-character SHA reachable from `main`, mark the run as
   explicit recovery/inspection, and later refuse any fresh-release path for it.
3. Retrieve this `github.run_id`'s original `created_at` through the Actions API
   and derive the candidate release date in `America/Los_Angeles`. This date is
   authoritative for a normal fresh candidate. For explicit recovery/inspection,
   it is diagnostic only; any existing nightly identity must be loaded from and
   verified against that release's tag and manifest.
4. Read the Cargo base version from `Cargo.toml` at the selected immutable source
   SHA, not from the workflow's protected `main` checkout or live `main`. Read
   the live `main` head separately.
5. Reject a selected SHA that is not reachable from `main`. For a publishing
   normal-candidate run, classify it as obsolete without mutation if it is no
   longer the live `main` head when preparation begins. A dry run may still
   build that event SHA for diagnostics but must label it stale.
6. Read `refs/heads/nightly` if it exists, determine its ancestry relationship
   to the selected SHA, and verify the complete published nightly release for
   the branch SHA before treating that pointer as a known-good newer nightly.
7. Search nightly tags and releases by both deterministic tag and full target
   SHA. This catches a partial draft created for the same SHA on an earlier date.
8. Verify release state, prerelease/latest flags, manifest, asset inventory, and
   checksums before classifying an existing release as complete. Resolve and peel
   its `tag_name` to the selected full SHA; as with official releases, do not use
   `target_commitish` as proof. When explicit `source_sha` finds existing state,
   adopt its already-recorded date, tag, and displayed version only after proving
   they match the deterministic identity; never rename it with the recovery
   dispatch's current date.
9. Select exactly one state and action from this table:

| Existing state | Required action |
| --- | --- |
| Complete published prerelease for SHA and `nightly` equals SHA | Successful no-op |
| Complete published prerelease for SHA and branch is absent or a verified ancestor | Skip builds; enter guarded create-or-fast-forward branch-repair path |
| Complete published prerelease for SHA and branch points to a verified published descendant | Successful obsolete no-op; never move the branch backward |
| Complete published prerelease for SHA and branch is divergent or its release cannot be verified | Fail closed as branch/release corruption |
| Branch equals SHA but release is absent, draft, or incomplete | Fail closed and require recovery |
| Any partial draft, orphan tag, duplicate release, or conflicting tag for SHA | Fail closed and report exact tag/release IDs for recovery |
| Published release targets another SHA under the deterministic tag | Fail closed as corruption; never overwrite or retarget |
| No release/tag state exists and branch points to a verified published descendant | Successful obsolete no-op; do not build or publish the older SHA |
| No release/tag state exists, branch is absent or a verified ancestor, selection is normal, and selected SHA is live `main` | Build and publish normally |
| No release/tag state exists for an explicit `source_sha` | Refuse fresh publication; explicit historical selection is recovery/inspection only |
| No release/tag state exists and branch is divergent or unverified | Fail closed as branch-state corruption |

10. Produce state/action, selection mode, version, tag, date, 12-character
    display SHA, full SHA, live-main SHA, branch object ID and ancestry state,
    release ID when present, and archive-name outputs.

The same-SHA idempotent no-op requires both a fully verified published release
and the correct branch pointer. An obsolete no-op requires a fully verified
published nightly for the descendant branch SHA. A matching branch or tag by
itself is never success. Branch repair may create an absent pointer once or update
an existing pointer only when the update is a true fast-forward.

Scheduled workflows may be delayed. Naming and idempotency must derive from the
selected SHA and original-run date rather than assuming the job began or was
rerun at an exact wall-clock minute. A new dispatch on a later date is a new
candidate identity, but if that SHA already has a complete published nightly the
state table makes it a verified no-op. If an earlier attempt created no tag,
draft, or release state, a new later-date dispatch may build the new identity
only when that event SHA is still live `main`; an explicit historical
`source_sha` remains recovery/inspection-only.

### 10.3 Nightly version reporting

Change `src/version.rs` so official behavior remains the default while CI can
inject a nightly identity at compile time:

```rust
pub const AGENT_VERSION: &str = match option_env!("CODEX_WARP_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
```

Requirements:

- Add tests for default official reporting and the formatting/helper logic used
  by nightly builds.
- Validate the workflow-generated override as valid SemVer before building.
- The override must affect `--version` and the upstream user agent consistently.
- Do not edit or commit `Cargo.toml` for nightlies.
- Ensure clean CI rebuilds cannot reuse a cache compiled with the wrong version;
  include channel/version in cache keys or disable reuse of final binary output.

### 10.4 Nightly packaging

Nightly cannot use an official SemVer tag that must equal `Cargo.toml`, so it
uses a small checked-in packaging script/workflow while sharing the same target
and archive-content contract as dist.

Requirements:

- Build with `cargo build --release --locked` and the compile-time nightly
  version override using the exact `rust-toolchain.toml` pin from the selected
  source SHA.
- Use the same four initial target triples.
- Copy the same runtime/config/documentation files as official archives,
  including the source SHA's exact `LICENSE`, `NOTICE`, and `CHANGELOG.md`.
- Use the same Unix/Windows archive formats.
- Generate SHA-256 files.
- Generate a deterministic `sha256.sum` containing exactly the four archive
  checksums, matching the official channel's unified checksum contract.
- Produce a machine-readable manifest containing the immutable nightly tag and
  identity date, full source SHA, base Cargo version read from that source,
  displayed nightly version, target, archive, checksum, exact Rust toolchain,
  `Cargo.lock` digest, runner image, workflow run URL, protected workflow SHA,
  packaging-contract digest, packaging-script digest, and every reviewed
  external packaging-tool digest.
- Define the nightly packaging contract as a canonical ordered list of the files
  and configuration inputs that determine archive names, archive contents,
  formats, checksums, manifest fields, and smoke tests. Compute its digest from
  the selected source SHA and record it in the manifest; recovery must reproduce
  and verify that digest rather than silently using today's contract from
  `main`.
- Attest every archive with GitHub artifact attestations. Historical and
  publication verification must constrain the certificate identity to the
  reviewed `Nightly` or `Nightly Recovery` signer workflow, `refs/heads/main`,
  the expected source or control digest, and GitHub-hosted runners; repository
  scope alone is insufficient.
- Add a CI parity check that compares the nightly target/content contract with
  the dist configuration so the two channels cannot silently drift.
- Require the same generated inventory contract as official releases: four
  target archives, one checksum output for each, `sha256.sum`, one manifest, and
  no forbidden artifact. Attestations remain GitHub-hosted; initial delivery does
  not attach a bundle that would silently change this inventory.

### 10.5 Nightly tests

For every target:

- Run the unpacked binary with `--version` and assert the full nightly version.
- Run `--help`.
- Verify all required archive paths exist.
- Verify the checksum against the archive.
- Verify no forbidden files are present.

On Linux, additionally run the normal locked unit tests before packaging. The
nightly job is not a replacement for the repository's full PR preflight or
mutation gate.

### 10.6 Nightly publication transaction

1. Complete every build and smoke test in jobs that do not declare the
   `release-automation` environment.
2. Start a separate final mutation job only after those jobs succeed. It retains
   the workflow-level `nightly-release` concurrency lock and declares the
   protected `release-automation` environment. Environment protection therefore
   gates the job before it reaches a runner; it is not treated as a step-level
   transition.
3. In that environment-bearing job, reread live `main`, `refs/heads/nightly`, and
   relevant releases immediately before the first mutation. A normal fresh
   publication requires the selected SHA still to equal live `main` and must
   stop as an obsolete no-op if a verified newer nightly branch/release already
   exists. Any divergent or unverified ref state fails closed. An explicit
   historical `source_sha` cannot enter the fresh-publication path. Use no
   repository mutation credential during this classification.
4. Before the private key is read or a tag exists, upload and attest a retained
   fresh-publication intent record. Bind it to the workflow/run attempt, control
   workflow SHA, selected source SHA, deterministic tag/date/version, proof that
   the tag and release were absent, expected inventory and digests, branch state,
   and approved mutation-plan digest. Give it the longest repository-supported
   retention and stop without mutation if it cannot be retained and verified.
5. Only then pass the private key to the pinned token action and mint one
   short-lived App token narrowed to `contents: write` and `workflows: write`.
   Reread the tag/release absence immediately before using the Git references
   create endpoint to create `refs/tags/<nightly-tag>` at the selected full SHA.
   Accept `201` only provisionally. On `422`, reread and report the competing
   value, but fail closed without creating a draft even when it equals the
   selected SHA: this run's pre-tag intent does not prove which actor won the
   race. Never create an annotated or moving nightly tag.
6. Resolve and peel the new tag to the selected full SHA, then upload and attest
   a retained tag-creation receipt bound to the intent digest, API result, tag,
   peeled SHA, run/attempt, and post-create reread. Give it the same maximum
   retention, record both evidence expiry times, and verify retrieval before
   proceeding. If the tag exists but this receipt cannot be retained, stop for
   documented human break-glass; no routine workflow may infer origin from tag
   existence alone.
7. Reread release absence and create one draft GitHub prerelease for the already
   existing immutable tag. Do not rely on `target_commitish` to create or prove
   the tag. Record and verify the exact draft release ID and `tag_name`.
8. Upload all archives, checksums, and manifest. Record GitHub-hosted attestation
   identities; do not attach an attestation bundle in initial delivery.
9. Verify the asset set against the manifest and again resolve/peel the existing
   tag to the selected full SHA.
10. Publish that exact release ID with `prerelease=true` and `latest=false`.
11. Immediately reread `refs/heads/nightly`. Refuse the update if it points to a
   newer published nightly or is not an ancestor of the selected source SHA.
12. If `refs/heads/nightly` is absent, use the Git references create endpoint to
   create it at the selected source SHA exactly once. A pre-create reread must
   still return `404`. Treat `201` as provisional success; if creation returns
   `422`, reread the ref as a possible race. Accept only an exact selected-SHA
   match, report a verified published descendant as an obsolete-pointer outcome
   without moving it, and fail closed for every other value. Reread after a
   successful create and verify the exact object ID and published release.
13. If the branch exists as a verified ancestor, use the same release App token
    to perform a normal, non-force, fast-forward-only update. Prefer a Git push
    whose advertised old object ID gives the server an atomic ref update; if an
    API without an old-object precondition is used, set `force=false`, reread
    immediately afterward, and fail/report any mismatch. Never force-push.
14. Write a workflow summary with the release URL, intent and tag-receipt
    evidence identifiers, branch action
    (`created`, `fast-forwarded`, `already-equal`, or `newer-descendant`), and
    final branch SHA, then allow the App-token action to revoke the token.

If publication succeeds but branch creation/update transiently fails, the
recovery job may safely retry the create-if-absent or fast-forward operation after
verifying the published release's target SHA. It must never force the branch to
an unrelated commit.

If fresh tag creation succeeds but draft creation fails, the immutable tag is an
expected recoverable orphan only while the originating run plus its verified
attested fresh-publication intent and tag-creation receipt are retained. Continue
it through Section 11.2's evidence-bound `recover-orphan-tag` operation. Once a
draft exists, use `resume-draft` or `replace-unpublished-draft` according to its
verified state. Missing or expired origin evidence remains human break-glass; it
never authorizes tag deletion, movement, or reuse.

### 10.7 Nightly release notes

Each nightly body should contain:

- a prominent unstable-build warning;
- full source SHA and link;
- build date and workflow run;
- displayed nightly version;
- comparison link from the previous successful nightly, if present;
- comparison link from the latest official release;
- supported targets;
- checksum and attestation verification instructions;
- statement that runtime configuration files are included in archives;
- link to the official latest release for users who want stability.

Do not auto-delete old nightlies in the initial implementation. Retention can
become a later explicitly reviewed policy; immutable unique tags are more useful
for auditability and source-exact rebuilding than a moving release.

## 11. Recovery and Manual Operations

### 11.1 Official recovery workflow

Provide a dedicated `.github/workflows/release-recovery.yml`. It must be present
on protected `main` and invoked with `workflow_dispatch` from `main`, so a fix to
the recovery logic does not rerun the possibly broken workflow stored at the
release tag.

Inputs:

- existing official tag;
- expected full SHA;
- expected existing draft release ID;
- operation: `rebuild-draft`, `resume-upload`,
  `replace-unpublished-assets`, or `publish-verified-draft`;
- for `resume-upload` only, required originating workflow name, run ID, run
  attempt, and retained artifact/evidence-manifest digest; these inputs must be
  empty for every other operation;
- required confirmation string equal to the tag for non-destructive operations,
  or exactly `replace-unpublished-assets:<tag>:<release-id>` for replacement.

Guards:

- tag must match `^v[0-9]+\.[0-9]+\.[0-9]+$`;
- tag version must match `Cargo.toml` at the tag;
- release must be the exact supplied ID, be a draft for mutation/publish
  operations, have `published_at=null`, and have no prior publication history;
- release `tag_name` must match the input tag, and the recursively peeled tag
  commit must match the expected full SHA; never use `target_commitish` as proof;
- published official assets are never overwritten or deleted;
- asset deletion is available only through `replace-unpublished-assets`, only
  for the supplied never-published draft, and only for exact asset IDs whose
  remote state/digest disagrees with the fully rebuilt expected inventory;
- recovery cannot create a different version or retarget a tag.

Execution model:

1. Record and checkout the immutable recovery `github.workflow_sha` selected by
   the protected-`main` dispatch, verify the dispatch ref is `main`, and prove
   that control commit remains an ancestor of live protected `main`.
2. Resolve and peel the immutable tag to the supplied full source SHA. Checkout
   that tagged source into a separate directory for source/contract validation
   and, only for an operation that rebuilds, compilation; never build the binary
   from `main`, and never compile during `resume-upload`.
3. Use orchestration, permission checks, recovery guards, and Action pins from
   protected `main`, but load the release inputs from the tag: `Cargo.toml`,
   `Cargo.lock`, `rust-toolchain.toml`, runtime configuration files, target
   matrix, archive contract, dist version, and recorded dist-tool digests. This
   preserves the artifact contract that was approved for that release.
4. If the tagged release contract or tool pin is itself the defect, add a
   reviewed `tools/recovery-recipes/official-<tag>.json` on `main`, validated against
   `tools/recovery-recipes/schema.json`. It must name the exact tag, original and
   replacement values, expected inventory difference, rationale, and
   expiry/removal condition; generic silent fallback to today's contract is
   forbidden. If it replaces dist or any manifest-producing tool, it must also
   bind the original and replacement tool versions, immutable binary source URLs
   and digests, original and replacement manifest-schema source URLs and digests,
   and the checked-in path
   `tools/recovery-recipes/schemas/<sha256>.json` of the vendored replacement
   schema. The filename digest must equal the file digest. Mutable/latest
   schema URLs and a replacement tool without its exact schema are invalid. The
   recovery recipe validator selects that replacement schema explicitly; it may
   never silently validate replacement output with the original or current-main
   schema.
5. Record workflow SHA, source SHA, tagged contract/toolchain digests, resolved
   compiler/Cargo versions, dist version and manifest-schema digest, and any
   original/replacement tool/schema/contract values and recipe digest in the
   workflow summary and the recovery-produced
   `codex-warp-release-metadata.json`. Recovery never adds a third manifest or
   separate recovery asset; the sidecar remains the single project-owned metadata
   document and must describe the actual recovered bytes.
6. Acquire the same tag-specific concurrency group used by the normal official
   dist finalizer, with `cancel-in-progress: false` and `queue: max`, so normal
   publication and recovery cannot mutate one draft concurrently.
7. Select exactly one evidence-production path before any release mutation:
   `rebuild-draft` and `replace-unpublished-assets` compile and verify all target
   outputs into new temporary GitHub Actions artifacts from the tagged source;
   `publish-verified-draft` may do the same when the remote draft is not already
   complete; `resume-upload` must not compile. Instead, its dedicated read-only
   job uses the supplied run ID/attempt to download the exact retained artifacts
   and evidence manifest from an allowlisted normal official or official-recovery
   workflow in this repository. It verifies the originating workflow/ref, tag,
   peeled source SHA, release-contract/tool/schema digests, artifact IDs/names/
   digests, both metadata documents, sidecar `mode=official`, and attestations
   before exposing those bytes to the mutation job. Missing, expired, superseded,
   or contradictory retained evidence fails with an instruction to dispatch
   `rebuild-draft`; it never silently rebuilds under `resume-upload`.
8. Inventory the existing draft read-only and calculate an exact missing/
   mismatched-asset plan. `rebuild-draft`, `resume-upload`, and
   `publish-verified-draft` fail closed on every existing mismatched asset and
   never delete or overwrite it. For `publish-verified-draft`, rebuild is
   optional, but every existing asset, checksum, both metadata documents,
   attestation, peeled tag commit, and tag/version relationship must verify
   before upload or undraft. Unpublished draft asset download uses the bounded
   App token because GitHub hides those assets from contents:read `GITHUB_TOKEN`.
   `replace-unpublished-assets` requires
   a complete rebuild and may select only exact mismatched asset IDs—including a
   documented GitHub upload remnant in `starter` state—from the never-published
   draft. Before the private key is read, retain, attest, retrieve, and verify a
   pre-delete intent binding the release/tag/SHA, remote asset IDs/names/states/
   digests, replacement digests, complete expected name/digest inventory,
   retained recovery-plan digest, recovery-recipe digest, run/attempt, control
   workflow SHA, and mutation-plan digest. No wildcard or name-only deletion
   plan is valid.
9. Start a separate mutation job, dependent on every read-only build/verification
   job, that declares the protected `release-automation` environment. Once its
   environment rules pass, revalidate the exact live draft/ref state in that job;
   only then pass the private key to the token action and mint the recovery-only
   App token with `contents: write` and `workflows: write`. Ordinary operations
   upload only missing assets whose expected name, checksum, target, version, and
   source SHA have already been verified. The replacement operation rechecks the
   attested intent and exact asset IDs, deletes only that enumerated set, rereads
   their absence, retains and attests a post-delete receipt containing every API
   result, and then uploads the verified replacements. If receipt retention
   fails, stop before re-upload; the retained intent explains the now-missing
   assets and a later non-destructive `resume-upload`, bound to that exact
   replacement run/attempt and retained evidence manifest, may restore only those
   exact expected bytes. In every case, reread and verify the complete remote
   inventory before publication.
10. With the same token, publish only the existing draft release ID and let the
    token revoke at job completion. Recovery cannot create a release,
    delete a release, create/move a tag, or change the draft's target. Asset
    deletion authority expires when that bounded replacement job ends.

The generated dist workflow remains the normal path. Recovery may reuse its
checked-in packaging commands or invoke pinned dist build/plan operations, but
it must not dispatch the historical tag workflow as its repair mechanism.

### 11.2 Nightly recovery

Add a dedicated `.github/workflows/nightly-recovery.yml`. It must exist on
protected `main`, accept dispatches only from `refs/heads/main`, and use the same
workflow-level `nightly-release` concurrency group as normal nightly publication,
with `cancel-in-progress: false` and `queue: max`.

Inputs:

- existing nightly tag in exact `nightly-YYYYMMDD-SHA12` form;
- expected full 40-character source SHA;
- expected release ID, or the literal `absent` only for `recover-orphan-tag`;
- operation: `resume-draft`, `replace-unpublished-draft`,
  `recover-orphan-tag`, or `repair-branch`;
- originating publication/replacement workflow run ID and attempt, required for
  `resume-draft` and `recover-orphan-tag` and rejected for other operations;
- exact originating nightly manifest SHA-256, required for `resume-draft` and
  rejected for every other operation; and
- required confirmation string. It equals the tag for non-destructive operations
  and exactly `replace-unpublished-draft:<tag>:<release-id>` for replacement.

The initial job is read-only and must:

1. Verify the workflow ref is `main`, the expected SHA is reachable from live
   `main`, the tag date/SHA12/full SHA relationship is exact, and the tag peels
   recursively to that full SHA.
2. Search releases/tags by both exact tag and full target SHA, reject duplicates
   or conflicting state, and resolve the exact release ID without trusting
   `target_commitish`.
3. Read the base version, `Cargo.lock`, `rust-toolchain.toml`, package inputs, and
   canonical nightly contract from the selected source SHA. When an existing
   manifest is present, treat its contract, packaging-script, lockfile, tool,
   toolchain, and original workflow-source digests as authoritative and reproduce
   them exactly. When the failed transaction never uploaded a manifest, derive
   those frozen values from the selected source SHA, record that manifest-absent
   basis in the plan, and write the derived values into the recovered release. It
   must never substitute the current `main` Cargo version, compiler, or packaging
   contract.
4. If a frozen contract, script, tool digest, or toolchain pin is itself the
   defect, require a reviewed
   `tools/recovery-recipes/nightly-<tag>.json` on protected `main`, validated
   against `tools/recovery-recipes/schema.json`, before rebuilding. Use the same
   schema as official recovery: exact tag/source SHA,
   original and replacement values, expected inventory/content difference,
   rationale, recipe digest, and expiry/removal condition. The recovery summary
   and manifest must record both original and replacement digests. Replacement
   of a manifest-producing tool also requires the same exact immutable original/
   replacement schema sources, digests, and vendored replacement-schema path as
   official recovery; no mutable or implicit schema fallback is valid. A recipe
   may repair outputs for the immutable tag but cannot change its date, displayed
   version, source SHA, archive names, or publication history. If corrected bytes
   conflict with an existing draft asset, only the guarded
   `replace-unpublished-draft` operation may discard that never-published release
   object; `resume-draft` still refuses overwrite.
5. For any rebuild, build and smoke-test the entire expected inventory into
   temporary workflow artifacts before an environment secret or mutation token
   is used. The displayed version must exactly reproduce the existing tag's
   date/base-version/SHA identity.
6. Produce a mutation plan containing operation, tag, source SHA, release ID,
   release state, branch state, expected asset inventory, existing asset
   inventory, every relevant original/replacement/recipe digest, and the exact
   allowed mutations.

The recovery workflow must use two explicit source trees. Checkout the immutable
`github.workflow_sha` selected by the protected-`main` dispatch into a control
directory, record it, and prove it remains an ancestor of live `main`; all
permission checks, state classification, orchestration, and mutation guards
execute from that directory. Checkout the expected historical source SHA into a different
source directory for Cargo inputs, the frozen packaging contract, and builds.
Use `persist-credentials: false` for both. Prove reachability with the GitHub
compare API or with an explicit fetch that contains the required `main` history
and tags; do not rely on `actions/checkout`'s default one-commit, no-tags fetch.
Record the control SHA, source SHA, and proof method in the mutation plan.

Operation contracts:

- `resume-draft` requires an exact failed normal Nightly run and attempt whose
  tag creation and draft creation succeeded but final publication did not. It
  authenticates the retained intent, tag receipt, candidate artifact,
  attestations, and supplied manifest SHA-256; it never rebuilds. The exact
  release must remain a never-published draft, the tag must point at the expected
  SHA, and every existing asset must match the retained candidate digest. Upload
  only missing assets; never overwrite or silently replace an existing asset.
  Verify the complete remote inventory byte-for-byte against that candidate,
  then publish and perform the guarded branch create/fast-forward transaction
  from Section 10.6.
- `replace-unpublished-draft` is the only automated destructive nightly recovery
  operation. It requires the exact confirmation string, exact release ID, an
  existing draft with no publication timestamp, a correct immutable tag, and a
  completely verified replacement build before mutation. Before deletion, emit
  and attest an orphan-recovery record as a retained workflow artifact. It must
  bind the originating workflow run ID, operation, exact release ID, proof that
  `published_at` was null immediately before deletion, tag, peeled source SHA,
  branch state, expected inventory and digests, and the approved mutation-plan
  digest. Give this evidence artifact the longest repository-supported retention,
  record its expiry in the summary, and never claim automated recovery after it
  expires. Delete only that draft release object, never the tag. Immediately
   after a successful delete and post-delete reread, emit and attest a second
   deletion-receipt artifact bound to the same run and plan digest before trying
   to recreate the draft. Give it the same maximum retention, record both expiry
   times, and verify retrieval. Recreate a draft for the existing tag, upload the
  verified inventory, reread and verify it, publish it, and then perform the
  guarded branch transaction. If deletion succeeds but either evidence artifact
  cannot be produced, fail closed to human break-glass. If recreation later
  fails, only that recorded failed transaction may be continued by
  `recover-orphan-tag` while its run and both attested records remain available.
- `recover-orphan-tag` is not a generic orphan-republication path. It accepts
  exactly two evidence classes. A failed normal fresh-publication transaction
  must supply its retained, verified attested pre-tag intent and tag-creation
  receipt. A failed replacement transaction must supply its retained, verified
  attested orphan-recovery intent and deletion receipt. In both cases require the
  exact originating workflow run and attempt, prove that the later draft
  creation/recreation and publication did not succeed, verify the exact immutable
  tag and peeled SHA, and require no current release for that tag or SHA. The
  input tag, source SHA, inventory, contract/tool/recipe digests, and mutation-plan
  digest must equal both records; the replacement evidence must additionally
  agree on the deleted release ID.
  It may create a draft for that existing tag, upload the verified inventory,
  verify, publish, and perform the guarded branch transaction. It cannot create,
  move, delete, or recreate a tag. Missing, incomplete, expired, unverifiable,
  unexplained, or wrong-class orphan evidence requires documented human break-glass
  investigation; absence of a current GitHub Release is never proof that no
  release was published previously.
- `repair-branch` requires a fully verified published prerelease and performs no
  build or release mutation. A read-only job first verifies the release, peeled
  tag, manifest, assets, checksums, and branch ancestry. Only then may a separate
  `release-automation` job mint the narrowed `contents: write` plus
  `workflows: write` App token and run Section 10.6's absent-branch creation or
  existing-branch fast-forward path, reread the result, and revoke the token.

All release mutations must occur in a separate job that depends on the read-only
plan/build jobs and declares the protected `release-automation` environment.
After the job starts, it revalidates live release/ref state and only then passes
the private key to the token action to mint one short-lived App token narrowed to
`contents: write` and `workflows: write`; it rechecks again immediately before
its first mutation and revokes the token at completion. Recovery may never
overwrite an asset, move or delete a nightly tag, mutate a published release, or
create a fresh historical tag. A wrong-SHA or otherwise conflicting protected
tag is corruption requiring documented human break-glass investigation; routine
automation does not repair it. This preserves the categorical rule that nightly
tags are never moved, deleted, or reused.

The normal `nightly.yml` manual dispatch with `source_sha` remains inspection and
verified no-op/branch-repair capable, but partial release repair must use the
dedicated workflow. An explicit historical SHA with no existing release or tag
state cannot create a fresh nightly. Every recovery search must use the full
target SHA as well as the tag so yesterday's partial state cannot be bypassed by
generating a new date-based tag today.

### 11.3 Failure states

- Any failed Release Please run: inventory the exact tag/release/PR state before
  choosing a retry; workflow failure or missing final outputs are not proof that
  no mutation occurred. If neither tag nor release exists, rerun the workflow to
  continue PR reconciliation normally.
- Release Please forced-tag creation followed by missing-draft failure: verify the
  exact release PR/tag/peeled SHA and use only the tested same-workflow Release
  Please continuation from Section 7.3, which runs
  `scripts/create-missing-official-draft.sh` and skips opening a newer version.
  Once it creates the one correct draft, rerun dist or use official existing-draft
  recovery. Do not use `gh release create`.
- Official build failure: draft remains unpublished; fix infrastructure or use
  guarded recovery.
- Official upload failure: draft remains. Upload only missing assets through
  ordinary resume; a `starter` remnant or wrong-digest asset requires the exact
  evidence-backed `replace-unpublished-assets` operation and can never authorize
  release/tag mutation.
- Publish failure: retry finalizer only after verifying the exact eleven-file
  inventory, both metadata documents, attestations, checksums, and SHA.
- Nightly build failure: no release and no branch movement.
- Nightly tag creation followed by draft-creation failure: branch does not move;
  continue only through `recover-orphan-tag` while the originating normal run's
  attested fresh intent and tag receipt remain valid.
- Later nightly publish failure: draft may remain; branch does not move.
- Nightly replacement deletion followed by recreation failure: automated orphan
  continuation is allowed only while the originating run plus its attested intent
  and deletion-receipt records verify; otherwise use documented human
  break-glass investigation and do not infer history from release absence.
- Branch create/update failure after nightly publication: verify and retry only
  the Section 10.6 create-if-absent or fast-forward operation.

## 12. Documentation Changes

Update:

- `README.md`
  - replace build-from-source as the only path with official archive downloads;
  - link stable and nightly channels;
  - label nightlies as unsupported/unstable;
  - explain that archives include runtime configs;
  - retain source-build instructions.
- `docs/development.md`
  - document release architecture and artifact targets;
  - document local dist validation and generated-workflow checks;
  - document official and nightly smoke tests;
  - document GitHub's public-repository scheduled-workflow inactivity behavior,
    the external 36-hour liveness monitor and its owner/notification destination,
    its newest-run `created_at` cadence rule, separate failure/stuck-run tests,
    and the exact alert-test/enable/diagnostic/manual-dispatch procedure.
- `AGENTS.md`
  - retain the rule that normal changes do not bump versions;
  - name Release Please as the official version owner;
  - state that nightly build metadata does not change Cargo versions.
- PR template/contributing guidance
  - document title convention and breaking-change footer;
  - explain how titles affect changelog and versioning.
- `SECURITY.md` or release verification section
  - document SHA-256 and `gh attestation verify` usage.
  - distinguish the schema-valid dist manifest from the attested Codex Warp
    release-metadata sidecar and show how to verify their digest/inventory links.
  - describe the initial guarantee as source-exact, traceable, and rebuildable
    from recorded inputs; explicitly disclaim later byte equality across mutable
    GitHub-hosted runner images.
  - document the reviewed dist-tool digest file and why remote installer scripts
    are not executed directly.
- `CHANGELOG.md`
  - keep Release Please compatible formatting and introductory prose.

## 13. Repository File Change Map

Expected new files:

```text
.github/workflows/release-please.yml
.github/workflows/release.yml               # generated dist workflow + overlay
.github/workflows/nightly.yml
.github/workflows/nightly-recovery.yml
.github/workflows/release-recovery.yml
.release-please-manifest.json
release-please-config.json
dist-workspace.toml
CHANGELOG.md
scripts/check-pr-title.sh
scripts/check-pr-title-harness.sh
scripts/release-please-policy-harness.sh      # invokes locked tool workspace
tools/release-please-policy/package.json
tools/release-please-policy/package-lock.json
tools/release-please-policy/harness.mjs
tools/release-please-policy/config.schema.json
tools/release-please-policy/fixtures/*.json
tools/release-automation-policy.json           # trusted PR/App identity + file contract
tools/release-automation-policy.schema.json
rust-toolchain.toml
scripts/check-release-readiness.sh
scripts/check-release-readiness-harness.sh
scripts/generate-dist-workflow.sh             # pinned generate + policy overlay
scripts/patch-dist-workflow.sh
scripts/check-dist-workflow.sh
scripts/install-pinned-dist.sh
tools/dist-tool-digests.sha256
tools/dist-manifest.schema.json
scripts/generate-release-metadata.sh
tools/release-metadata.schema.json
tools/recovery-recipes/README.md              # reviewed tag-specific schema/process
tools/recovery-recipes/schema.json
tools/recovery-recipes/schemas/*.json         # digest-named replacement schemas
scripts/package-nightly.sh                   # or platform-specific equivalents
scripts/check-release-contract.sh
```

Expected modified files:

```text
Cargo.toml                                   # release/distribution metadata only
Cargo.lock                                   # when Release Please bumps versions
src/version.rs
src/version_tests.rs
.github/workflows/ci.yml
.github/dependabot.yml                       # monitor Actions if appropriate
.github/pull_request_template.md
scripts/ci-change-scope.sh
scripts/ci-change-scope-harness.sh
scripts/source-checks.sh
README.md
docs/development.md
AGENTS.md
```

The implementation must inspect repository scripts before editing this map;
existing harness patterns should be extended rather than duplicated.

## 14. Implementation Phases

### Phase 0 — live audit and branch preparation

- [ ] Back up this plan to a version-controlled project/issue/docs location and
      record the link in this header.
- [ ] Re-read `AGENTS.md` and all release/CI-relevant repository guidance.
- [ ] Record live `main` SHA, Cargo version, workflows, settings, protection,
      tags, and releases.
- [ ] Confirm no concurrent release-automation PR exists.
- [ ] Create a normal implementation branch from live `main`.
- [ ] Run the existing full local preflight before changes to establish a clean
      baseline.
- [ ] Record the intended Release Please and dist versions and their full Action
      SHAs.
- [ ] Select a dedicated non-production sandbox repository for destructive
      official and nightly transaction/convergence proofs. Record its owner,
      cleanup policy, ruleset/environment settings parity, and a separate
      sandbox-only App/secret path with the same narrowed permissions. Never
      install the production App key in the sandbox and never use production-
      looking test tags in `jatmn/Codex-warp`.
- [ ] Record the exact Release Please dependency from the pinned Action commit,
      pinned Node LTS patch version, and immutable platform-specific dist archive
      digests. Fetch the `dist-manifest` schema from that exact dist release,
      record its upstream URL and digest, and reject a latest/mutable schema URL.
- [ ] Select and record one exact stable Rust patch version for
      `rust-toolchain.toml`; inventory required components and confirm every
      target runner can install it.
- [ ] Resolve and record the full source commit and digest for the Release Please
      config schema that matches the pinned dependency; no `main` schema URL may
      enter deterministic CI.
- [ ] Before implementing either publication channel, use the pinned Release
      Please Action/dependency and equivalent draft/forced-tag settings in the
      sandbox to construct the exact correct-tag/no-release state after a merged
      release PR. The pinned Action will not POST a GitHub draft when that tag
      already exists; prove `scripts/create-missing-official-draft.sh` in the
      same workflow creates exactly one unpublished draft for the existing
      unmoved tag and skips proposing a newer version. Then prove existing-draft
      continuation can finish it. Record the immutable Action/dependency,
      settings, run IDs, tag/release IDs, and cleanup receipt.
- [ ] Select the external nightly-liveness monitor, owner, read-only access model,
      and maintainer notification destination. Confirm it can preserve the newest
      scheduled run's `created_at`, query completed conclusion separately, and
      detect a six-hour queued/in-progress run; record any cost or account
      dependency before treating daily publication as an operational guarantee.

Exit criterion: baseline and pinned inputs are current, clean, and recorded; the
architecture-blocking Release Please convergence proof has passed before release
workflow implementation begins.

### Phase 1 — release contract and version identity

- [ ] Seed `CHANGELOG.md` with the Release Please-compatible project
      introduction required by every official and nightly archive; validate it
      as part of the archive-content contract before packaging work begins.
- [ ] Add the exact `rust-toolchain.toml` pin and make existing local/CI source
      checks consume it without a competing runner-default or `stable` override.
- [ ] Add compile-time nightly version override with tests.
- [ ] Define archive names, required contents, forbidden contents, and targets;
      require `LICENSE`, `NOTICE`, and `CHANGELOG.md` from the exact source SHA.
- [ ] Add reusable validation helpers for archive inventory and version smoke.
- [ ] Add the versioned `codex-warp-release-metadata.json` schema plus generation
      and cross-manifest validation fixtures. Prove the dist manifest remains
      unmodified and schema-valid, the sidecar rejects missing or contradictory
      source/toolchain/runner/contract fields, and the two documents cannot claim
      different artifact inventories. Cover both explicit modes: official mode
      requires a real recursively peeled tag and non-implicit dist announcement;
      PR-upload-proof mode requires implicit announcement, complete PR/build
      identity, and `publishable=false`. Prove every finalizer, recovery, and
      release-upload validator rejects proof-mode metadata.
- [ ] Vendor the exact pinned dist-manifest schema and validate offline against
      it; prove its recorded digest and version match the dist executable pin.
- [ ] Add the shared official/nightly tag-specific recovery-recipe schema,
      naming convention, digest rules, expiry requirements, and fixtures that
      reject any identity/source/publication-history mutation. A recipe that
      replaces dist or a manifest-producing tool must bind and vendor the exact
      replacement manifest schema URL/digest; reject missing, mutable, unvendored,
      or tool/schema-mismatched replacements.
- [ ] Add PR-title checker and harness.
- [ ] Add the release-automation policy schema and readiness-classifier harness.
      Cover exact repository/base/head/App-bot/file-set fields plus ordinary and
      automation-shaped partial-match fixtures; populate the installed App's live
      identity later in Phase 4 through protected review.
- [ ] Add the pinned Release Please version-policy fixture for every accepted
      commit category, scoped forms, both breaking syntaxes, malformed breaking
      footer, Dependabot, and `Release-As`; verify bump/no-release and changelog
      placement, including the explicit `revert` patch policy.
- [ ] Vendor the matching pinned Release Please config schema, point the editor
      URL at its full source commit, and validate CI against the vendored copy
      without network access.
- [ ] Verify the fixture uses Release Please's pinned parser/versioning logic and
      isolated synthetic histories, not a locally reimplemented bump algorithm.
- [ ] Commit the isolated harness package lock, run it with the pinned Node patch
      and `npm ci --ignore-scripts`, and prove package/API drift fails closed.
- [ ] Integrate title validation into required Source Checks.
- [ ] Update CI change-scope classification/harnesses for release files.

Exit criterion: version/channel behavior and release contract pass locally with
no publishing capability.

### Phase 2 — dist packaging proof

- [ ] Add Cargo package metadata needed by dist (`description`, `repository`,
      `readme`, and related non-publishing metadata).
- [ ] Add `dist-workspace.toml` with initial matrix and archive includes.
- [ ] Generate the official dist workflow with pinned Action commits.
- [ ] Add `allow-dirty = ["ci"]` plus the deterministic concurrency-overlay and
      regeneration-check scripts; prove the overlay is idempotent and fails on
      upstream template drift.
- [ ] Prove the checked workflow uses dist-native, versionless official archive
      names and derives each Unix tar root from the archive basename.
- [ ] Replace generated remote dist installer execution with the digest-verified
      platform archive installer; prove the checked workflow contains no
      unverified dist download or `curl | sh` path.
- [ ] Prove the overlay removes the generated root `contents: write` grant,
      leaves every `GITHUB_TOKEN` read-only, replaces the broad dist tag glob,
      and performs the strict stable-tag runtime check before downloads or builds.
- [ ] Verify the overlay keeps build and collection/local-verification jobs
      read-only and outside `release-automation`, then gives only the dependent
      live-state-revalidating official mutation job the short-lived App token
      narrowed to `contents: write` and `workflows: write`.
- [ ] Prove the collection job preserves the final dist manifest byte-for-byte
      and generates/validates the separate project metadata sidecar from verified
      target evidence. Prove a dependent non-environment job redownloads and
      revalidates the sidecar, attests it without embedding a recursive self-
      digest, and has no App token or contents-write permission.
- [ ] Confirm its only publication trigger is an App-created tag matching exact
      stable `vMAJOR.MINOR.PATCH` syntax and that `create-release = false` is
      reflected in the generated plan.
- [ ] Prove generated remote hosting is split into credential-bearing upload,
      remote verification, and undraft steps; allow the pinned non-mutating
      `dist host` manifest-preparation invocation, but fail regeneration if its
      behavior changes or one credential-bearing command/step can both upload and
      publish.
- [ ] Add the custom generate-overlay-compare check to source validation.
- [ ] Run `dist plan` and a local Linux official-style archive build.
- [ ] Temporarily set `pr-run-mode = "upload"` on the implementation PR,
      regenerate/overlay the workflow, and run its exact generated build and
      collection path on Linux, both macOS targets, and Windows. Prove all
      environment, App-token, hosting, tag, and release mutation jobs are skipped
      structurally on the PR event.
- [ ] Download the resulting four official-style workflow artifacts, inspect
      their trees, validate checksums, the unmodified dist manifest, the project
      metadata sidecar, their cross-document inventory, and attestations, and run
      the platform-appropriate extracted-archive smoke tests. Confirm `NOTICE` is
      present with the selected source digest and configs remain nested/usable.
- [ ] Confirm the PR-run dist manifest has
      `announcement_tag_is_implicit=true` and the sidecar has exactly
      `mode=pr-upload-proof`, `publishable=false`, and the recorded PR/base/head/
      checkout/run identity. Prove the proof sidecar claims no official tag,
      peeled tag SHA, or release ID, and prove the official upload/finalizer
      validator refuses it.
- [ ] Materialize the complete official-shaped PR proof directory and prove the
      shared helper's `pr-upload-proof` profile accepts exactly eleven files and
      rejects a missing sidecar, modified dist manifest, second sidecar, or any
      unexpected file. Prove the same directory is rejected by the
      `official-publication` profile.
- [ ] Return `pr-run-mode` to steady-state `plan`, regenerate/overlay again, and
      require a clean byte comparison. Preserve the successful upload-run ID and
      artifact inventory as rollout evidence.

Exit criterion: the exact generated official workflow has built and smoke-tested
all four official-style archives and both metadata documents without creating a
tag, GitHub Release, environment deployment, or mutation token.

### Phase 3 — nightly dry run

- [ ] Add nightly workflow with schedule disabled or dry-run-only initially.
- [ ] Add `NIGHTLY_PUBLISH_ENABLED=false`, the explicit publish expression, and
      tests for scheduled/manual/missing-input combinations.
- [ ] Prove repository variables are compared as strings, the prepare output is
      normalized, and every mutation guard compares it exactly with `'true'` or
      uses `fromJSON`; reject bare string-output guards.
- [ ] Prove manual dispatch from any ref other than `main` cannot reach a secret
      or mutation step, and prove explicit `source_sha` cannot create fresh
      historical nightly state.
- [ ] Build every target without GitHub release or branch mutation.
- [ ] Verify displayed version, exact pinned Rust toolchain, archives (including
      `NOTICE`), checksums, manifest, and attestations.
- [ ] Verify rerunning the same workflow run preserves its original-run date and
      exact identity; separately verify a later new dispatch follows the
      documented same-SHA state rules.
- [ ] Exercise every eligibility-state-table row with mocked/read-only release
      metadata, including an earlier-date partial draft for the same SHA, a
      verified newer descendant branch/release, and divergent/unverified branch
      state.
- [ ] Run release-contract parity check against dist configuration.

Exit criterion: all nightly artifacts are downloadable as workflow artifacts
and pass smoke tests, with no Git ref or GitHub Release mutation. Workflow
artifacts and repository-scoped GitHub attestations are expected service records,
not violations of this dry-run criterion.

### Phase 4 — nightly publication

- [ ] Create/install the repository-scoped release App with the permissions in
      Section 8 before any nightly mutation is enabled.
- [ ] Query the installed production App bot account through the read-only API and
      record its stable numeric account ID, login, `type=Bot`, exact Release
      Please head branch, repository/base identity, and allowed release-PR file
      contract in `tools/release-automation-policy.json` through the normal
      protected PR path. Reread and fixture-test the committed values before
      Release Please is enabled.
- [ ] Before any official `v*` ref is allowed into `release-automation`, create
      the Section 8.3 `v*` creation/immutability ruleset pair. Retrieve it through
      the read-only API and prove its IDs, targets, enforcement state, rules, and
      bypass actors exactly match the sandbox-proven policy. Do not create a
      production probe tag or issue a test Git-ref mutation. Retain only the
      documented human break-glass bypass on the immutability ruleset.
- [ ] Add the dedicated protected-main nightly recovery workflow and exercise all
      four operation contracts using mocked/read-only state before enabling its
      mutation job. Prove the separate control/source checkout model and require
      both orphan evidence classes: synthetic verified fresh-publication
      intent/tag-receipt records from a failed normal transaction and verified
      pre-delete/deletion-receipt records from a failed replacement. Prove
      wrong-class, unexplained, incomplete, or expired evidence fails closed, and
      prove `actions: read` is confined to the prepare/evidence-producing or
      evidence-reader jobs while no recovery `GITHUB_TOKEN` has contents write.
- [ ] Deploy the exact candidate nightly and recovery workflows to the sandbox
      with its separate App, environment, and Section 8.3 ruleset pairs. Exercise
      real mutations for a normal publication and all four recovery operations.
      In particular, inject a stop immediately after the retained tag receipt,
      verify the draft is absent and branch did not move, then complete the orphan
      through `recover-orphan-tag`. Prove missing/altered evidence fails, recovery
      never moves/recreates the tag, and the sandbox branch advances only after
      publication. Retain run/evidence/settings/cleanup receipts. Production
      mutation remains disabled until this end-to-end proof passes.
- [ ] Add the App client-ID variable and the private-key environment secret;
      configure `release-automation` for `main` first, verify the official tag
      ruleset pair, and only then add protected official `v*` tag refs. Verify an
      arbitrary branch/tag cannot enter it and record the settings snapshots that
      prove there was no unprotected-tag activation window.
- [ ] Configure the separate production `nightly` writer/integrity branch
      rulesets and `nightly-*` creation/immutability tag rulesets. Retrieve and
      compare their exact live settings with the sandbox-proven policy; do not
      make a production probe ref or destructive test call. Verify guarded
      creation/fast-forward and denied delete/force-push/tag-move behavior from
      the retained sandbox mutation evidence.
- [ ] Enable manual non-dry-run publication.
- [ ] Before the first production publication, prove `nightly` is absent and
      recheck the sandbox evidence for the one-time create-reference path,
      post-create reread, and racing-creation failure handling. Do not create the
      production branch ahead of a published nightly.
- [ ] Publish the first production nightly through the normal path with no
      failure injection. Confirm it creates exactly one immutable tag, publishes
      one complete prerelease, and only then exercises the one-time create path
      for `nightly` and rereads the resulting ref.
- [ ] Confirm GitHub does not mark it Latest.
- [ ] Confirm `nightly` points at the exact released source SHA.
- [ ] Rerun and verify it no-ops.
- [ ] Prove a queued stale run exits before build/publication when a verified
      newer nightly exists, and prove branch repair never moves backward.
- [ ] Advance `main` through a reviewed `.github/workflows/` change and prove the
      guarded nightly App token can publish/update the release and fast-forward
      `nightly` while all earlier jobs remain read-only.
- [ ] Enable the daily off-minute schedule.
- [ ] Configure the external read-only monitor outside this repository to query
      scheduled Nightly runs at least every 12 hours. Use newest-run `created_at`
      for the 36-hour cadence test, evaluate completed conclusion separately, and
      alert on the documented six-hour queued/in-progress threshold. Prove a late
      completion cannot reset a stale cadence clock.
- [ ] Record the public-repository 60-day inactivity limitation, monitor owner,
      endpoint, notification destination, credential rotation if any, exact
      workflow-enable command, and guarded manual fallback in the maintainer
      runbook; trigger and acknowledge a synthetic liveness alert.
- [ ] Change `NIGHTLY_PUBLISH_ENABLED` to `true` only after the published manual
      nightly and recovery/no-op checks pass.

Exit criterion: one complete nightly exists and the branch advances only after
success.

### Phase 5 — Release Please enablement

- [ ] Add Release Please config and manifest, connect them to the changelog seed
      created in Phase 1, and prove the first release PR updates that file without
      replacing its durable introductory prose.
- [ ] Add pinned release workflow with draft/forced-tag behavior.
- [ ] Run Release Please CLI dry-run against live history.
- [ ] Run the version-policy fixture and record expected results.
- [ ] Verify the Release Please Action/dependency and draft/forced-tag settings
      still exactly match the successful Phase 0 sandbox convergence evidence.
      Rerun that proof before enablement if any pinned byte, dependency, setting,
      permission, or relevant GitHub behavior has changed; drift or failure blocks
      official enablement.
- [ ] Merge configuration through the normal protected PR path.
- [ ] Confirm the App opens a release PR and required CI runs on it.
- [ ] Confirm the opened PR's repository/base/head, creator ID/login/type, and
      complete changed-file inventory exactly match the committed release-
      automation policy; any mismatch blocks enablement rather than reclassifying
      the PR as ordinary.
- [ ] Confirm the proposal is `0.1.0` and curate initial notes.
- [ ] Install and validate the guarded official recovery workflow before the
      release PR is merged.
- [ ] Confirm recovery dispatches from protected `main`, validates tagged source,
      rebuilds it only for rebuild-capable operations, reuses exact retained
      run/attempt evidence only for `resume-upload`, consumes the selected tagged
      release contract/tool/schema digests, records both SHAs and contract digest,
      and shares the official tag concurrency lock.
- [ ] In the sandbox, exercise actual official recovery against a never-published
      draft containing both a simulated `starter` upload remnant and a wrong-
      digest uploaded asset. Prove `resume-upload` refuses them, the exact
      destructive confirmation is required, `replace-unpublished-assets` retains
      and verifies its pre-delete intent/post-delete receipt, deletes only the
      enumerated asset IDs, and restores a completely verified draft. Also prove
      the operation refuses any published release or tag/ref mutation.
- [ ] In the sandbox, stop a normal official run after verified artifacts are
      retained but before all draft uploads complete. Prove `resume-upload`
      requires the exact workflow/run/attempt/evidence digest, performs no
      compilation, restores only missing matching bytes, and rejects a different,
      expired, missing, or altered artifact set with an instruction to use
      `rebuild-draft`. Prove `rebuild-draft` performs a new tagged-source build.
- [ ] Re-audit the already-active `v*` tag ruleset pair and environment ref policy
      immediately before the release PR can be merged; fail the rollout if the
      release App is not the sole routine stable-tag creator or if rewrite,
      deletion, or unprotected environment entry is possible.
- [ ] Prove normal/recovery official finalization and the guarded nightly mutation
      transaction use narrowly scoped App tokens with `workflows: write`, while
      the bounded Release Please PR/tag token also includes `workflows: write`
      for GitHub's workflow-file ref/release rule. All build/collection jobs omit
      it, and no token is available outside its owning bounded job.
- [ ] Add a static permission-policy test proving the Release Please token requests
      `workflows: write`, and document GitHub's 403/404 symptom when that
      permission is missing during tag/draft creation across workflow-file
      changes. Do not create a throwaway stable-looking tag to test this on the
      production repository.
- [ ] Add and require the release-readiness check; prove an outstanding official
      draft blocks the authenticated Release Please PR while ordinary PRs remain
      unaffected. Prove branch-only, App-author-only, fork-head, wrong-base,
      changed-event-actor, and unexpected-file cases cannot bypass or impersonate
      Release Please classification.
- [ ] Inspect the generated dist tag filter, App-created-event behavior, bounded
      draft lookup, and fail-closed path as one handoff readiness review.

Exit criterion: a passing, human-reviewable `v0.1.0` release PR exists; no
official release has been published yet.

### Phase 6 — first official release

- [ ] Review version, Cargo lockfile, changelog, and expected tag.
- [ ] Merge the Release Please PR.
- [ ] Immediately verify there was no older official draft/tag inconsistency and
      that the readiness check passed for this release PR.
- [ ] Confirm the App-created tag event started exactly one dist workflow.
- [ ] Treat that legitimate tag as the production positive ruleset-creation test:
      re-read the exact unchanged tag and the live creation/immutability rulesets,
      compare them with the retained sandbox evidence, and confirm no production
      probe tag was created.
- [ ] Confirm draft/tag/Cargo SHA and version association.
- [ ] Confirm identity by recursively peeling the tag to the expected commit;
      do not treat the release object's `target_commitish` as authoritative.
- [ ] Observe all dist target builds and smoke tests.
- [ ] Verify the exact eleven-file asset inventory, SHA-256 files, source-exact
      `NOTICE`, unmodified/schema-valid dist manifest, and project metadata
      sidecar values against the tagged `rust-toolchain.toml` pin and source
      digests.
- [ ] Verify at least one artifact attestation independently.
- [ ] Confirm the draft becomes published only after all gates.
- [ ] Download one archive per operating system where practical and smoke-test.
- [ ] Confirm GitHub `releases/latest` resolves to `v0.1.0`, not a nightly.

Exit criterion: `v0.1.0` is complete, source-exact, traceable, documented, and
installable under the Section 9.1 rebuildability scope.

### Phase 7 — hardening

- [ ] Re-audit the existing `v*` and `nightly-*` creation/immutability ruleset
      pairs and the `nightly` writer/integrity pair after their first real
      publications; tighten actor/bypass lists without preventing creation of new
      unique tags or normal branch fast-forwards by the guarded finalizers.
- [ ] Enable GitHub immutable releases only after verifying compatibility with
      draft-first publication.
- [ ] Re-test official `rebuild-draft`, `resume-upload`,
      `replace-unpublished-assets`, and `publish-verified-draft` recovery against
      never-published sandbox drafts while proving a published release is
      immutable. Prove rebuild compiles anew while resume consumes only exact
      retained run/attempt artifacts and fails closed after their retention
      expires.
- [ ] Test nightly branch repair for an already published nightly.
- [ ] Test nightly `resume-draft`, `replace-unpublished-draft`, and
      evidence-bound `recover-orphan-tag` recovery without modifying the
      immutable tag. Test both permitted orphan origins: failure after fresh tag
      creation using the normal run's intent/tag receipt, and failure after draft
      deletion using the replacement run's intent/deletion receipt. Reject when
      either required record is missing, expired, mismatched, or from the wrong
      transaction class.
- [ ] Test official and nightly tag-specific recovery recipes against the checked
      schema, including rejection of recipes that alter identity, source SHA, or
      publication history and successful repair of a deliberately defective
      historical packaging/tool input. For a replacement manifest-producing
      tool, prove the exact vendored replacement schema/digest is selected and an
      original, current-main, mutable, or mismatched schema is rejected.
- [ ] Add Dependabot coverage for pinned Actions where supported.
- [ ] Review workflow logs for secret exposure and excessive permissions.
- [ ] Document maintainer runbooks and ownership.

Exit criterion: routine releases and common recovery paths need no ad hoc token,
tag rewrite, or manual asset editing.

## 15. Validation Matrix

### 15.1 Local/static validation

- Existing `bash scripts/ci-preflight.sh` passes.
- Rust formatting, Clippy, tests, build, docs, deny, and audit behavior remain
  consistent with repository policy.
- `git diff --check` passes.
- YAML is parsed and linted with the repository-approved method.
- JSON/TOML configuration parses.
- The project release-metadata schema and fixtures validate offline; generation
  preserves the final dist manifest byte-for-byte, rejects cross-document
  inventory/source/toolchain contradictions, and produces exactly one sidecar.
  Official mode requires a real recursively peeled tag and non-implicit dist
  announcement. PR-upload-proof mode requires complete PR/build/run identity,
  implicit announcement, and `publishable=false`; every publication/recovery
  entry point rejects proof mode.
- Recovery-recipe schema and positive/negative fixtures validate offline;
  channel/tag filename mismatch, missing original/replacement digest, identity
  change, missing expiry, or publication-history mutation fails. Replacement of
  dist or another manifest-producing tool additionally fails when the exact
  immutable replacement schema source/digest is missing, mutable, unvendored, or
  mismatched to the replacement tool.
- The committed Release Please config validates against the vendored schema whose
  digest and source commit match the pinned Release Please dependency; validation
  performs no mutable-ref or network fetch.
- The checked-in release-automation policy validates offline and the readiness
  harness proves exact genuine-App classification, ordinary-PR bypass of the
  release-state query, fail-closed automation-shaped partial matches, and complete
  changed-file API pagination.
- The policy harness uses the pinned Node patch, committed npm lockfile, and the
  exact Release Please dependency from the pinned Action commit.
- `rust-toolchain.toml` names one exact stable patch version; local CI, generated
  dist, nightly, and recovery paths contain no runner-default, floating `stable`,
  or conflicting dist toolchain selection.
- Release Please CLI dry-run reports intended version/changes.
- Release Please policy fixtures prove the expected bump/no-release outcome and
  changelog placement for every accepted type (`feat`, `fix`, `perf`, `revert`,
  `refactor`, `docs`, `test`, `build`, `ci`, `chore`), scoped forms,
  `build(deps)`, both breaking syntaxes, a malformed breaking footer, and
  `Release-As` histories.
- The pinned dist generate-overlay-compare check is clean, and the resulting
  workflow contains the official tag concurrency lock, exact stable-tag filter
  plus runtime regex, a job-level tag/event guard on every publication job before
  any `release-automation` environment, read-only `GITHUB_TOKEN` permissions, only
  digest-verified dist installation paths, and distinct credential-bearing
  upload, remote-verify, and undraft steps. The pinned non-mutating `dist host`
  manifest invocation is permitted only while source inspection proves it cannot
  access the mutation token; only the guarded official mutation job receives the
  narrowed official-publication App token.
- The retained Phase 2 PR upload run proves the generated official build and
  collection jobs produced all four target archives while every environment,
  hosting, App-token, tag, and release mutation job was skipped; final
  steady-state configuration is regenerated with `pr-run-mode = "plan"`. Its
  dist manifest marks the announcement implicit and its sole project sidecar is
  non-publishable PR-upload-proof mode; an official finalizer fixture rejects it.
- Static workflow policy proves every official/nightly publication path performs
  collection and local verification in non-environment jobs, then uses one
  dependent `release-automation` mutation job that revalidates live state before
  the private key is passed to the pinned token action.
- Static permission policy proves `actions: read` is confined to the nightly
  prepare job, normal mutation evidence check, and recovery evidence-reader; the
  normal nightly mutation job has only the evidence-related additions to its
  read-only `GITHUB_TOKEN`, and no recovery `GITHUB_TOKEN` has contents write.
  Only the official/nightly destructive replacement operations receive their
  narrowly scoped evidence-attestation permissions.
- Static permission policy also confines `pull-requests: read` to the Release
  Please readiness classifier and proves its changed-file API pagination fails
  closed on truncation or incomplete results.
- Title checker harness covers valid, invalid, scoped, breaking, Dependabot,
  and Release Please titles.

### 15.2 Artifact validation

- All four expected archives exist with unique names.
- Official archive names use dist's native `codex-warp-<target>` form; their
  names and tar roots do not contain a separately injected release version.
- Tar top-level layout and ZIP root layout match the release contract exactly.
- Every archive contains source-exact `LICENSE`, `NOTICE`, and `CHANGELOG.md`;
  their digests match the immutable selected source.
- Executable permission is preserved in Unix archives.
- Windows archive contains `.exe` and correct path separators.
- `--version` is exact for official and nightly channels.
- `--help` succeeds from extracted archives.
- Required configs are present and nonempty.
- SHA-256 verification succeeds.
- `sha256.sum` exists, contains exactly one valid entry for each of the four
  archives, and contains no additional path.
- The unmodified dist manifest's tag/version/target inventory matches the build
  and validates against the schema emitted by the exact pinned dist version, or
  against the exact immutable replacement schema selected by a valid tag-specific
  recovery recipe when that recipe replaces the manifest-producing tool.
- An attached official project metadata sidecar has `mode=official`,
  `publishable=true`, a non-implicit dist announcement, and source SHA, real tag,
  dist-manifest digest, target map, `rustc -Vv`, Cargo, lockfile, runner/native-
  tool observations, and contract/schema digests that match the build and
  selected source's exact `rust-toolchain.toml` pin. PR-upload-proof mode is never
  accepted as a release asset.
- The attached official asset inventory matches the union of the pinned dist-
  generated manifest and higher-level contract: four archives, four per-archive
  checksum files, one unified `sha256.sum`, one unmodified dist manifest, one
  project metadata sidecar—eleven files total—and no installer, source tarball,
  second metadata document, or unexpected artifact. GitHub source links and
  hosted attestations are verified separately.
- Attestation verification succeeds with `gh attestation verify`.

### 15.3 State-transition validation

- Non-release push only updates/creates Release Please PR.
- Release PR merge creates one official tag and one draft.
- Every official/recovery path resolves and peels `tag_name` to the expected full
  commit SHA; changing or trusting `target_commitish` cannot satisfy the guard.
- The App-created official tag starts exactly one dist run; nightly tags start
  none.
- In the sandbox, the exact correct-tag/no-release state after a merged release
  PR converges under the pinned Release Please workflow to one draft for the
  existing unmoved tag and exact release-PR SHA; failure of this proof blocks
  official enablement.
- Failed build cannot publish official draft.
- A GitHub upload remnant in `starter` state or any wrong-digest asset prevents
  ordinary official resume/publish. Only `replace-unpublished-assets` with the
  exact draft confirmation and attested asset-ID plan may delete those assets;
  it cannot target a published release, delete a release, or mutate a tag.
- Successful build publishes once.
- Repeated run cannot retag or overwrite a published official release.
- Nightly on unchanged SHA is a no-op only when the published prerelease and
  branch pointer are both complete and verified.
- The first successful nightly creates an absent `nightly` ref once through the
  create-reference path, verifies it afterward, and never treats absence as an
  update from a fabricated old object ID.
- A branch-only match, partial draft, orphan tag, conflicting release, or
  earlier-date draft for the same SHA fails closed and reports recovery IDs.
- Overlapping scheduled/manual nightlies serialize; a stale run cannot replace
  a newer `nightly` branch pointer or publish an older nightly after a verified
  newer descendant.
- Retrying the same nightly tag produces the same displayed binary version.
- A historical nightly rebuild reads the Cargo base version and canonical
  packaging inputs from the selected source SHA and must match the manifest's
  contract, script, lockfile, toolchain, tool, and workflow-source digests unless
  an exact schema-valid tag-specific recipe records and justifies every
  replacement digest.
- Rerunning the same workflow run preserves the original-run date; a later new
  dispatch follows the explicitly documented same-SHA state rules.
- Failed nightly does not move `nightly`.
- Successful nightly publishes prerelease then advances branch.
- Nightly recovery can resume a correct partial draft, replace only a
  never-published draft release object while retaining its immutable tag, recover
  an orphan tag only from one of the two evidence-bound origins (normal fresh
  intent/tag receipt or replacement intent/deletion receipt), or repair the
  branch. It cannot infer publication history from current release absence,
  accept the wrong evidence class, overwrite assets, or
  create/move/delete/reuse a tag.
- Tag-specific official/nightly recovery recipes are schema-valid, bind exact
  original/replacement digests, tool/schema authority, and identity, and cannot
  change source SHA, tag/version/date/archive names, or publication history. A
  manifest-producing tool replacement uses only its exact immutable vendored
  replacement schema.
- Nightly never changes GitHub Latest.
- Scheduled nightly publication requires the string comparison
  `vars.NIGHTLY_PUBLISH_ENABLED == 'true'`; downstream string outputs are never
  used as bare truthy guards, and missing manual inputs never authorize
  publication.
- Manual Release Please and Nightly dispatches from a ref other than `main`
  cannot access the App private key or mutate repository state; an explicit
  historical nightly `source_sha` cannot create a fresh release.
- A reviewed workflow-file change can cross the guarded official/nightly
  release and ref mutations using only the final short-lived App tokens; every
  `GITHUB_TOKEN` remains read-only for contents.
- Sandbox aggregate-ruleset mutation tests prove the sandbox App can create fresh
  `v*`/`nightly-*` tags and create/fast-forward `nightly`, but cannot update/
  delete an existing tag, delete the branch, or make a non-fast-forward branch
  update. Production activation uses read-only settings parity, makes no probe
  ref or test Git-ref mutation, and uses the first legitimate nightly and
  official tags as its positive creation observations.
- The Release Please App token alone includes its four documented permissions,
  including `workflows: write`; policy tests reject its omission and reject that
  permission on build or collection jobs.
- A newer Release Please PR cannot merge while an earlier official draft or
  incomplete official tag exists. Release Please workflow continuation may
  create the missing draft for the newest official tag when that tag exists
  with no release object; it rechecks that state with the App token so a hidden
  unpublished draft cannot be mistaken for absence. Only a same-repository PR
  whose exact base, configured head branch, recorded App bot creator, and allowed
  release-output files all match can enter Release Please readiness mode;
  automation-shaped partial matches fail closed and ordinary PRs remain
  unaffected.
- Official recovery runs workflow logic from protected `main`, validates the
  exact tagged source and either rebuilds it under the tagged/recipe-selected
  contract or resumes exact retained evidence according to the selected
  operation, uses the recovery-only App permission, and cannot race the normal
  finalizer for the same tag.
- Nightly recovery runs guards and orchestration from a protected-`main` control
  checkout, builds from a separate exact-source checkout, and proves historical
  reachability without relying on a shallow one-commit checkout.

## 16. Acceptance Criteria

The project is complete when all of the following are true:

- [ ] Every releasable merge updates one visible Release Please PR.
- [ ] PR titles are validated before merge.
- [ ] Merging the release PR is the only normal human action needed for an
      official release.
- [ ] Official versions in tag, Cargo manifest, binary output, changelog, and
      GitHub Release all agree.
- [ ] Official releases contain complete Linux, macOS, and Windows archives with
      runtime configs, source-exact `LICENSE`/`NOTICE`/`CHANGELOG.md`, and
      checksums.
- [ ] Every official release has exactly eleven attached assets: four archives,
      four per-archive checksums, `sha256.sum`, one unmodified/schema-valid dist
      manifest, and one attested `mode=official`, publishable project metadata
      sidecar whose real-tag/source/toolchain/schema/contract/inventory claims
      agree with the dist manifest and release. A PR-upload-proof sidecar can
      never satisfy this criterion.
- [ ] Before the first official tag, the exact generated dist workflow has built,
      collected, and smoke-tested all four target archives in a read-only PR
      upload run with every hosting/mutation job structurally skipped. Its dist
      announcement is implicit and its sidecar is explicitly non-publishable
      `pr-upload-proof` mode bound to the exact PR/build/run identity.
- [ ] The pinned Release Please version has passed the sandbox
      correct-tag/missing-draft convergence proof; the documented continuation
      `scripts/create-missing-official-draft.sh` creates one draft without
      moving/duplicating the tag or changing version. This proof is complete
      before Phase 1 unless later pinned/settings drift requires it to be
      repeated.
- [ ] An official release cannot publish with a missing required target.
- [ ] Official recovery handles GitHub `starter` remnants and wrong-digest assets
      only through evidence-backed `replace-unpublished-assets` against the exact
      never-published draft. Ordinary resume refuses them, and no recovery path
      deletes a published asset/release or mutates an official tag.
- [ ] Official `rebuild-draft` always compiles and verifies tagged source anew;
      `resume-upload` never compiles and accepts only exact retained artifacts
      bound to its supplied same-repository workflow/run/attempt/evidence digest.
      Missing, expired, or contradictory resume evidence fails closed rather
      than silently rebuilding.
- [ ] A new official release PR cannot merge while an earlier official tag or
      draft is incomplete. Release Please PR classification requires the exact
      same-repository base/head, configured automation branch, recorded App bot
      creator, and allowed release-output files; partial automation matches fail
      closed without imposing release-state API checks on ordinary PRs.
- [ ] Nightly runs daily while GitHub's scheduler is enabled and supports manual
      dispatch; an external read-only monitor checks it at least every 12 hours
      and notifies the named maintainer destination after 36 hours without a new
      scheduled run by `created_at`, after a failed scheduled completion, or
      after the documented six-hour stuck-run threshold. Completion/update time
      never resets cadence. The
      public-repository 60-day inactivity limitation, monitor ownership, tested
      alert, re-enable command, and guarded manual fallback are documented.
- [ ] Nightly binaries identify their date/SHA without changing Cargo
      version files.
- [ ] Nightly identity dates come from the immutable original run timestamp, so
      reruns after midnight cannot rename artifacts.
- [ ] `nightly` is never advanced before publication or moved backward; after a
      complete transaction it identifies that release, and any
      publish-before-branch failure is detected and repairable by a verified
      create-if-absent or fast-forward operation.
- [ ] Nightly releases are prereleases and never replace Latest stable.
- [ ] Published nightly tags cannot be moved or deleted by routine automation.
- [ ] Before production nightly mutation, the complete normal and injected
      tag-without-draft recovery transactions have succeeded with actual
      mutations in the non-production sandbox; the first production nightly uses
      the normal path without failure injection.
- [ ] Fresh nightly publication retains attested pre-tag intent before creating
      the immutable tag, retains an attested post-tag receipt before draft
      creation, and can recover a tag-without-draft state only from those two
      exact originating-run records.
- [ ] Release assets have verifiable build provenance.
- [ ] Workflows use least privilege and full-SHA Action pins.
- [ ] Separate aggregate rulesets allow the App to create release tags and
      create/fast-forward `nightly`, while denying that same App all existing-tag
      updates/deletions, branch deletion, and non-fast-forward branch updates.
      Mutation behavior is proven in the parity sandbox; production creates no
      probe tag and uses settings parity plus the first legitimate publications
      as positive creation evidence.
- [ ] A recovery recipe that replaces dist or another manifest-producing tool
      binds an immutable, vendored replacement manifest schema and its digest;
      recovery cannot use the original, mutable latest, or current-main schema by
      accident.
- [ ] Every downloaded dist executable is verified against a reviewed,
      repository-recorded digest before execution.
- [ ] No personal long-lived PAT or crates.io token is required.
- [ ] Retry and recovery procedures are documented and tested.
- [ ] Nightly recovery runs from protected `main`, shares the normal nightly lock,
      executes guards from a separate control checkout, freezes/builds the exact
      selected source's packaging contract from a source checkout, retains
      immutable tags, and implements the four explicit guarded operations in
      Section 11.2. Orphan continuation requires both attested records from one
      permitted evidence class—fresh intent/tag receipt or replacement
      intent/deletion receipt—and fails closed for mismatched classes or after
      they expire.
- [ ] Official and nightly recovery can use only schema-valid tag-specific
      recipes that record original/replacement digests without changing release
      identity, source, archive names, or publication history; a replacement
      manifest-producing tool is inseparable from its exact immutable vendored
      replacement schema/digest.
- [ ] Official recovery runs repaired workflow logic from protected `main`,
      validates immutable tagged source and either rebuilds under its selected
      tagged/recipe contract or resumes exact retained run evidence according to
      operation, records both SHAs/contract/schema digests, uses the recovery-only
      App permission, and shares the normal finalizer's tag lock.
- [ ] Release documentation describes artifacts as source-exact, traceable, and
      rebuildable from recorded inputs and explicitly does not promise later
      byte-for-byte equality on mutable GitHub-hosted runner images.
- [ ] README and developer documentation explain both channels.

## 17. Explicit Non-Goals for Initial Delivery

- Publishing the crate to crates.io.
- Automatic self-update inside Codex Warp.
- Homebrew, Winget, Chocolatey, Scoop, npm, MSI, or container publication.
- Code signing/notarization for macOS or Windows.
- Linux ARM64 or musl support before target-specific testing.
- Automatically deleting or rewriting old nightly releases.
- Automatically merging the Release Please PR.
- Releasing directly on every feature/fix merge.
- Supporting long-term maintenance release branches in the first iteration.
- Guaranteeing byte-for-byte rebuilds across later GitHub-hosted runner image or
  native SDK updates.

These can be separate projects after the stable release pipeline has operating
history.

## 18. Maintainer Runbook Summary

### Cut a normal official release

1. Review the open Release Please PR.
2. Confirm version and changelog. If notes need correction, use the documented
   commit-override procedure and wait for Release Please to refresh the PR.
3. Merge it.
4. Confirm the App-created tag starts the dist workflow and watch it through
   publication.
5. Verify one downloaded archive and its attestation.

### Force a specific next version

1. Use a reviewed commit/PR containing the documented
   `Release-As: X.Y.Z` footer.
2. Let Release Please update the release PR.
3. Never manually move or replace an existing official tag.

### Request a nightly

1. Run the Nightly workflow manually.
2. Leave `dry_run=true` for artifact inspection, or explicitly choose publish.
3. Dispatch only from `main`. Leave `source_sha` empty for a new nightly. Supply
   a full historical SHA only to inspect/recover nightly state that already
   exists for that SHA.
4. Confirm the selected event SHA is still live `main`; an obsolete run will
   report a no-op rather than publish an older nightly.

### Diagnose a missing scheduled nightly

1. Treat the external monitor notification as the normal detection path. If it
   reports no scheduled run `created_at` within 36 hours, a failed scheduled
   completion, or a run queued/in-progress beyond six hours, acknowledge the
   alert at the named maintainer destination and run
   `gh workflow view nightly.yml` and
   `gh run list --workflow nightly.yml --event schedule` before assuming the
   build itself failed.
2. For a public repository inactive for 60 days, GitHub may disable the schedule.
   Re-enable it with `gh workflow enable nightly.yml` or the Actions web UI and
   record that intervention.
3. Verify `NIGHTLY_PUBLISH_ENABLED` still has the intended lowercase string value.
   A disabled publication flag should yield a scheduled dry run, not silence.
4. If an artifact is needed immediately, use a guarded manual dispatch from
   `main`; do not weaken the ref, state, or publication guards.
5. After remediation, verify the next external poll clears the alert. Exercise a
   synthetic alert on the documented cadence so an untested notification path is
   never counted as liveness coverage.

### Recover a failed nightly

1. Resolve the exact nightly tag, full source SHA, release ID or orphan-tag state,
   branch SHA, asset inventory, and existing manifest before choosing an
   operation.
2. Use `resume-draft` only when the tag is correct and every existing draft asset
   matches; it uploads only missing assets.
3. Use `replace-unpublished-draft` only with the exact destructive confirmation
   string and only for a never-published draft. It replaces the release object,
   not the immutable tag.
4. Use `recover-orphan-tag` only as a continuation of one recorded failed
   transaction. For a failed normal fresh publication, supply its run/attempt and
   verify the retained attested pre-tag intent plus tag-creation receipt. For a
   failed replacement, verify its pre-delete intent plus deletion receipt, exact
   deleted release ID, and failed recreate/publish path. In either class, the
   tag/SHA, plan/inventory/contract/tool/recipe digests, and run identity must
   agree. Missing, cross-class, incomplete, or expired evidence requires human
   break-glass; current release absence alone is never sufficient. Use
   `repair-branch` only after fully verifying the published release and a
   create/fast-forward-safe branch state.
5. Confirm the recovery summary records the protected-main control/workflow SHA,
   separate source SHA, ancestry-proof method, exact Rust pin, all frozen
   contract/tool digests, and any schema-valid tag-specific recipe with both
   original and replacement digests.
6. Confirm guards/orchestration executed from the control checkout and builds
   used the separate exact-source checkout with no persisted checkout credential.
   Never delete, move, or recreate a nightly tag through routine automation;
   wrong-SHA tag state requires human break-glass investigation.

### Recover a failed official release

1. Resolve the exact tag, peeled SHA, release PR, originating Release Please run,
   draft release ID if present, and existing asset inventory.
2. If the correct protected tag exists but the draft is absent, do not invoke
   dist or create a release manually. Use only the pinned-version Release Please
   continuation proven in the sandbox, verify it creates one draft for the
   existing unmoved tag, and record the continuation run and release ID. If that
   tested path does not converge, stop official automation for reviewed redesign.
3. Fix the underlying workflow/configuration through a normal PR if needed.
4. Confirm the tagged release contract/toolchain/tool/schema digests are usable;
   if not, merge a schema-valid reviewed tag-specific recovery recipe on `main`.
   A replacement dist or manifest-producing tool requires its exact immutable
   vendored replacement schema URL/digest; never substitute the original,
   current-main, or a mutable latest schema.
5. If a new build from tagged source is required, invoke `rebuild-draft`. Use
   `resume-upload` only when the exact retained official workflow/run/attempt and
   evidence-manifest digest are known and still downloadable; it reuses those
   verified bytes without compiling and fails closed if they are unavailable or
   contradictory. Use `publish-verified-draft` only when the complete remote
   draft already verifies, or with its explicit optional rebuild path. None of
   those operations may delete or overwrite an asset.
6. If an exact never-published draft contains a GitHub `starter` remnant or a
   wrong-digest asset, use only `replace-unpublished-assets` with the exact draft
   ID and confirmation `replace-unpublished-assets:<tag>:<release-id>`. Verify the
   read-only plan names exact asset IDs and expected replacement digests before
   approving the environment job; never delete an asset manually in the UI.
7. Confirm replacement recovery retained and verified its pre-delete intent and
   post-delete receipt, deleted only the enumerated assets, and reports the
   source, workflow, contract, toolchain, both metadata documents, and optional
   recipe digests.
8. Publish only after the exact eleven-file inventory and all cross-manifest,
   checksum, SHA, and attestation relationships verify through the recovery-only
   App permission gate.

### Security rule

Never solve a release failure by force-moving an official tag, overwriting a
published asset, bypassing required checks, or placing a maintainer PAT in the
workflow. Never place the release App on a ruleset bypass list that also contains
the tag-update/tag-deletion or branch-force-push/deletion protections intended to
constrain that App.

## 19. Later Enhancements

After several successful official and nightly releases, evaluate separate
proposals for:

- embedding a default configuration or defining an OS-specific config search
  path, which would make binary-only installers practical;
- signed/notarized Windows and macOS binaries;
- Homebrew and Winget/Scoop distribution;
- Linux ARM64 and musl archives;
- crates.io publication with trusted publishing;
- a machine-readable update feed or opt-in self-updater;
- nightly retention/index pages;
- maintenance release branches for older stable versions;
- bit-for-bit reproducible archives using normalized archive metadata and
  compression, pinned build/native environments where feasible, and independent
  double-build hash comparisons on every supported target;
- SBOM generation and attestation.

Each enhancement must preserve the official Release Please gate and the rule
that published official tags/assets are immutable.
