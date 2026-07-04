# Public Release Security Plan

This checklist is for the moment Codex Warp moves from private to public.

Current private-repo state checked on 2026-07-04:

- Only `@jatmn` is a repository collaborator.
- Pending repository invitations are empty.
- Deploy keys are empty.
- Forking is enabled.
- GitHub Actions uses read-only default workflow permissions.
- GitHub Actions requires SHA-pinned actions.
- Merge commits, rebase merges, and auto-merge are disabled.
- Squash merge, web commit signoff, branch deletion after merge, and Dependabot
  security updates are enabled.
- `CODEOWNERS`, `SECURITY.md`, a pull request template, and Dependabot config
  are present in the repository.

## Release Window

Use a short, deliberate release window:

1. Confirm no unmerged private work, notes, local debug logs, or credentials are
   present in tracked files.
2. Confirm no pending collaborator invitations exist.
3. Confirm no write-capable deploy keys exist.
4. Make the repository public.
5. Immediately enable `main` branch protection.
6. Verify a non-collaborator can only contribute by fork and pull request.
7. Enable and review public-repository security features.

Do not invite outside collaborators before branch protection is active. Public
contributors should fork the repository and open pull requests from their forks.

## Pre-Public Checks

Run these before changing visibility:

```bash
git status --short --branch
git log --oneline --decorate -5
git diff --check HEAD
cargo fmt --check
cargo test --locked
cargo build --locked
target/debug/codex-warp --version
target/debug/codex-warp --help
```

Review tracked files for secrets or private references:

```bash
rg -n "api[_-]?key|token|secret|password|bearer|authorization|private|internal" \
  README.md AGENTS.md SECURITY.md docs configs src Cargo.toml Cargo.lock
```

Check GitHub access surfaces:

```bash
gh api repos/jatmn/Codex-warp/collaborators --paginate \
  --jq '.[] | {login, role_name, permissions}'

gh api repos/jatmn/Codex-warp/invitations --paginate \
  --jq '.[] | {invitee:.invitee.login, permissions}'

gh api repos/jatmn/Codex-warp/keys --paginate \
  --jq '.[] | {id, title, read_only}'

gh api repos/jatmn/Codex-warp/actions/permissions/workflow \
  --jq '{default_workflow_permissions, can_approve_pull_request_reviews}'
```

Expected:

- Only `jatmn` appears as a collaborator.
- Invitations output is empty.
- Deploy keys output is empty, or every key is read-only and intentional.
- Workflow permissions are read-only.
- Actions cannot approve pull request reviews.

## Immediate `main` Protection

After the repository is public, protect `main` before doing any other public
collaboration work. GitHub Free supports protected branches for public
repositories.

Required `main` settings:

- Require pull requests before merging.
- Require 1 approving review.
- Require CODEOWNERS review.
- Dismiss stale approvals when new commits are pushed.
- Require approval of the most recent reviewable push.
- Require status checks before merging.
- Require the `Source Checks` CI status.
- Require the branch to be up to date before merging.
- Require conversation resolution.
- Require linear history.
- Do not allow force pushes.
- Do not allow deletions.
- Apply restrictions to administrators if the option is available.

Recommended API command:

```bash
gh api --method PUT repos/jatmn/Codex-warp/branches/main/protection --input - <<'JSON'
{
  "required_status_checks": {
    "strict": true,
    "contexts": ["Source Checks"]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "required_approving_review_count": 1,
    "require_last_push_approval": true
  },
  "restrictions": null,
  "required_conversation_resolution": true,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
```

Verify:

```bash
gh api repos/jatmn/Codex-warp/branches/main/protection
```

## Fork-Only Contributions

Public users without repository write access cannot create branches directly in
`jatmn/Codex-warp`; they must fork and open pull requests. Keep that true by
maintaining these rules:

- Do not add outside collaborators with write, maintain, or admin access.
- Do not leave pending write invitations open.
- Do not add write-capable deploy keys.
- Do not install GitHub Apps that can push branches unless they are required and
  reviewed.
- Keep `CODEOWNERS` as `* @jatmn`.

If direct collaborators are ever added later, branch creation outside protected
patterns may become possible for those collaborators. At that point, prefer an
organization-owned repository with repository rulesets or explicit branch
creation restrictions.

## GitHub Security Features

After public visibility is active, review Settings > Security > Advanced
Security and ensure the public-repository features are enabled:

- Dependabot alerts
- Dependabot security updates
- Secret scanning
- Push protection
- Code scanning, if a Rust scanner workflow is added

The current repository already has Dependabot configuration for Cargo and
GitHub Actions. Add code scanning separately when there is a chosen scanner and
the workflow is reviewed.

## Actions Safety

Keep Actions boring and constrained:

- Keep default `GITHUB_TOKEN` permissions read-only.
- Keep repository-level SHA pinning required.
- Pin every third-party action to a full commit SHA.
- Do not add `pull_request_target` workflows unless the workflow never checks
  out or runs untrusted contributor code.
- Do not expose repository secrets to forked pull requests.
- Keep required CI job names unique; `Source Checks` is the required gate for
  this repository.

Verify:

```bash
gh api repos/jatmn/Codex-warp/actions/permissions \
  --jq '{enabled, allowed_actions, sha_pinning_required}'

gh api repos/jatmn/Codex-warp/actions/permissions/workflow \
  --jq '{default_workflow_permissions, can_approve_pull_request_reviews}'
```

Expected:

- `sha_pinning_required` is `true`.
- `default_workflow_permissions` is `read`.
- `can_approve_pull_request_reviews` is `false`.

## Post-Public Smoke Test

After protection is enabled, confirm the public workflow:

1. Open a test branch in a fork.
2. Submit a pull request to `jatmn/Codex-warp:main`.
3. Confirm CI runs with read-only token permissions.
4. Confirm the PR cannot merge until `Source Checks` passes.
5. Confirm CODEOWNERS requests `@jatmn`.
6. Confirm stale approval is dismissed after a new commit.
7. Confirm direct pushes to `main` are blocked or require the protected flow.

## Ongoing Maintenance

Weekly:

- Review Dependabot alerts and PRs.
- Review recent Actions runs.
- Confirm no new collaborators, invitations, or write deploy keys were added.

Before adding any workflow:

- Check whether it runs on forked pull requests.
- Check whether it requests write permissions.
- Check whether it touches secrets, release tokens, or publishing credentials.
- Pin third-party actions to full SHAs before merging.

Before adding any collaborator:

- Decide whether they really need write access.
- Prefer fork-based PRs for contributors.
- Re-check branch and ruleset coverage before granting access.

## References

- GitHub protected branches:
  <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches>
- GitHub security and analysis settings:
  <https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-security-and-analysis-settings-for-your-repository>
- GitHub Actions secure use:
  <https://docs.github.com/en/actions/reference/security/secure-use>
