# Release Automation Maintainer Runbook

This is the activation and incident runbook for Codex Warp release automation.
The checked-in policy defaults are fail-closed: do not enable a gate until its
preceding proof is complete and recorded in the release-automation pull
request.

The detailed design and acceptance matrix live in
[`release-automation-plan.md`](release-automation-plan.md). This runbook is the
short operational path, not a substitute for those gates.

## 1. Checked-in Control Plane

The important files are:

| Purpose | File |
| --- | --- |
| Release Please | `release-please-config.json`, `.release-please-manifest.json`, `.github/workflows/release-please.yml` |
| Official packaging | `dist-workspace.toml`, `.github/workflows/release.yml`, `tools/release-contract.json` |
| Nightly | `.github/workflows/nightly.yml`, `tools/nightly-manifest.schema.json` |
| Recovery | `.github/workflows/release-recovery.yml`, `.github/workflows/nightly-recovery.yml` |
| Identity/readiness | `tools/release-automation-policy.json` |
| Immutable pins | `tools/release-tooling.json`, `tools/dist-tool-digests.sha256`, `rust-toolchain.toml` |

Run the complete repository preflight before every release-automation commit or
push:

```bash
bash scripts/ci-preflight.sh
```

Regenerate the official workflow only through:

```bash
bash scripts/generate-dist-workflow.sh
bash scripts/check-dist-workflow.sh
```

Do not hand-edit `.github/workflows/release.yml`; the overlay generator must
reproduce it byte-for-byte.

## 2. GitHub App

Create a GitHub App dedicated to this repository, suggested name
`codex-warp-release-bot`, and install it only on `jatmn/Codex-warp`.

Repository permissions:

- Metadata: read
- Contents: read and write
- Pull requests: read and write
- Issues: read and write
- Workflows: read and write

Do not grant organization-wide access, administration, or a maintainer PAT.
Store the App client ID as the Actions variable
`RELEASE_APP_CLIENT_ID`. Store its private key only as the
`RELEASE_APP_PRIVATE_KEY` secret in the `release-automation` environment.

Query the installed App bot through the API and record its stable numeric ID,
login, and `type=Bot` in `tools/release-automation-policy.json`. Change
`enabled` to `true` only in the same reviewed activation pull request.

## 3. Protected Environment

Create an environment named `release-automation` without a normal-path human
reviewer. Allow deployments only from:

- protected branch `main`; and
- protected tags matching `v*`, but only after the official tag rulesets below
  are active and verified.

The environment secret is unavailable to build and read-only planning jobs.
Only dependent mutation jobs declare the environment, revalidate live state,
and then request a short-lived repository-scoped App token.

## 4. Rulesets

GitHub rules from multiple matching rulesets aggregate. Keep creation bypass
separate from immutability so the App can create a ref but cannot later rewrite
or delete it.

Create and test these in a dedicated sandbox first:

1. `v*` tag creation: restrict creation; bypass only the release App and a
   documented human break-glass actor.
2. `v*` tag immutability: restrict updates and deletion; no App bypass.
3. `nightly-*` tag creation: restrict creation; bypass only the release App and
   break glass.
4. `nightly-*` tag immutability: restrict updates and deletion; no App bypass.
5. `nightly` branch writer: restrict creation and updates; App and break-glass
   bypass.
6. `nightly` branch integrity: block force pushes and deletion; no App bypass.

Preserve `main` branch protection, required checks, linear history, review,
conversation resolution, and administrator enforcement. The App must not bypass
`main` review or its required checks.

In the parity-configured sandbox, prove with a narrowed App token that fresh
refs and fast-forward branch updates work while tag rewrites/deletions,
non-fast-forward branch moves, and branch deletion fail. Prove a normal write
collaborator cannot create or mutate protected release refs. Never use a
throwaway production tag as a ruleset probe.

## 5. Repository Variables

Create these variables with string value `false` first:

| Variable | Effect when `true` |
| --- | --- |
| `OFFICIAL_RELEASES_ENABLED` | Allows Release Please and the official dist finalizer to reach their protected mutation jobs. |
| `NIGHTLY_MUTATION_READY` | Allows a qualifying nightly run to mint the App token. |
| `NIGHTLY_PUBLISH_ENABLED` | Allows scheduled nightlies to request publication; manual runs still require `dry_run=false`. |
| `OFFICIAL_RECOVERY_READY` | Allows the official recovery mutation job. |
| `NIGHTLY_RECOVERY_READY` | Allows nightly recovery mutation or branch repair. |

The separate readiness and publication variables make it possible to prove the
complete build path without granting mutation. Do not use an unset variable as
an intentional true value.

## 6. Activation Order

1. Merge the implementation through the normal protected pull-request path.
2. Run the official pull-request upload proof for all four targets and verify
   the exact eleven-file candidate. Return cargo-dist to `pr-run-mode = "plan"`
   and regenerate before merge if upload mode was used for proof.
3. In the sandbox, exercise normal official/nightly transactions and every
   recovery operation, including injected failures after intent retention, tag
   creation, draft creation, upload, deletion, and publication.
4. Install the production App, record its verified bot identity, create the
   production rulesets, and compare them read-only with the sandbox policy.
5. Create the protected environment, variable, and secret configuration.
6. Set `NIGHTLY_MUTATION_READY=true`; keep scheduled publication false.
7. Manually run Nightly from `main` with `dry_run=true`, inspect all targets,
   then run once with `dry_run=false`. Confirm the prerelease, attestations,
   checksums, Latest behavior, and `nightly` branch. Rerun and confirm no-op.
8. Set `NIGHTLY_PUBLISH_ENABLED=true` only after the manual publication proof.
9. Set `OFFICIAL_RECOVERY_READY=true` only after its sandbox campaign passes.
10. Set `OFFICIAL_RELEASES_ENABLED=true` only after the pinned Release Please
    forced-tag/missing-draft continuation proof passes in the sandbox. That
    proof uses `scripts/create-missing-official-draft.sh` because the pin does
    not POST a GitHub draft when the Git tag already exists.
11. Confirm Release Please opens one internal App-authored release PR and that
    the readiness classifier recognizes its exact creator/head/base/files.
12. For the bootstrap only, ensure the setup squash commit includes
    `Release-As: 0.1.0`; inspect and curate the proposed `0.1.0` notes.

`NIGHTLY_RECOVERY_READY` remains false until the corresponding sandbox recovery
campaign is complete.

## 7. Normal Operations

To cut an official release, review and merge the Release Please PR. Do not
manually create its tag or publish its draft. Confirm the App-created tag starts
one Release workflow, all four builds pass, the eleven assets verify, and the
draft becomes Latest only after finalization.

To request a nightly without mutation, dispatch Nightly from `main` with
`dry_run=true`. For a manual publication, select `main`, set `dry_run=false`,
and leave `source_sha` empty. A historical `source_sha` is inspection or
verified branch-repair input only; it cannot mint a fresh historical nightly.

## 8. Recovery

Never rerun a mutation blindly. First inventory the exact tag, recursively
peeled SHA, release ID/state, assets/digests, originating run/attempt, retained
evidence, and protected branch state.

Official Release Recovery accepts:

- `rebuild-draft`: rebuild all targets from the immutable tag and upload only
  missing assets;
- `resume-upload`: authenticate a retained candidate from the exact supplied
  run and attempt without compiling;
- `replace-unpublished-assets`: delete only mismatched assets from the exact
  never-published draft after retained, attested intent; and
- `publish-verified-draft`: publish an already complete, fully verified draft.
  Use it when the exact eleven assets are already on that never-published draft.
  If the normal tag workflow attached those assets but has not yet set
  `draft=false`, stop that publish job and continue through this operation
  instead of starting another official version.

Cancel-before-undraft is not proof that the draft is still unpublished. The
tag job may still PATCH `draft=false` after `gh run cancel` returns. Re-GET
the exact release ID and require `draft=true` and `published_at=null` immediately
before any asset delete or upload. Do not use `gh release upload` as a draft
mutation; if that GET already shows a published release, stop. Recovery cannot
repair published bytes. Cut the next official version instead. Prior-release
verification still fail-closes on a missing release, outstanding draft, or
active official run, but it allows a broken published official so Release Please
can open that next version. Published bytes cannot be repaired.

Official archive and metadata attestations are accepted only when their signed
identity names the reviewed `Release` tag workflow or `Release Recovery` main
workflow and the expected source/control digest. A repository-scoped
attestation from a pull-request or unrelated workflow is not release evidence.

It cannot create/delete a release, create/move/delete a tag, change a version,
or mutate a published release.

For `recover-orphan-tag`, force-cancel the origin Nightly as soon as the
immutable tag and `nightly-tag-creation-receipt` exist. Cooperative
`gh run cancel` can still POST the draft and upload assets. The draft step
must appear with a non-success conclusion and no release object.

For `replace-unpublished-assets`, inject the attested mismatch only after a
fresh GET shows `draft=true` and `published_at=null`, during
`attest-official-metadata` after the eleven assets exist and before
`publish-official-release` starts. Then force-cancel the origin tag job.
Do not inject after cancel if that GET already shows a published release.

Nightly Recovery accepts:

- `resume-draft`: authenticate the exact retained candidate from the supplied
  failed normal Nightly run/attempt and manifest SHA-256, then upload only
  missing verified assets to the same draft without rebuilding;
- `replace-unpublished-draft`: retain and attest intent, delete only the exact
  never-published draft object, retain and attest the deletion receipt, then
  recreate for the existing immutable tag;
- `recover-orphan-tag`: continue only an exact retained/attested normal or
  replacement transaction and its retained candidate; and
- `repair-branch`: verify the published prerelease and only create or
  fast-forward `nightly`.

Use the exact confirmation string shown by the workflow input. Replacement
operations are destructive and must never target a published release. Missing,
expired, conflicting, or unverifiable evidence stops automated recovery and
requires documented human break-glass investigation.

Recovery control jobs execute the immutable `github.workflow_sha` selected by
the `main` dispatch and prove that commit remains an ancestor of live protected
`main`; historical release source is materialized separately.

A tag-specific recipe under `tools/recovery-recipes/` is required if the frozen
contract/tool/schema itself is defective. The baseline workflows deliberately
fail if a recipe declares substitutions that have not also received a reviewed
workflow implementation; they never silently apply today's tools to historical
bytes.

## 9. Scheduled-Workflow Liveness

GitHub can disable scheduled workflows in a public repository after prolonged
inactivity. Configure an external, read-only monitor owned outside this
repository to check the latest successful Nightly run and alert the maintainer.
Record the monitor owner, credential rotation, alert route, and response target
in the activation pull request. The monitor must not receive release mutation
credentials.
