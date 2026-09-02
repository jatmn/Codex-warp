## Summary

-

<!--
Use a Conventional Commit title because squash merge makes it the main-branch
commit: type(optional-scope)!: concise description
Accepted types: feat, fix, perf, refactor, docs, test, build, ci, chore, revert
Templates and changelog mapping: AGENTS.md#commits
-->

## Contribution checklist

- [ ] I checked for duplicate or overlapping existing pull requests.
- [ ] This PR does not introduce a new implementation language.
- [ ] This PR does not add Python for any reason.
- [ ] I can respond to review feedback within one week.
- [ ] My PR title follows the repository's Conventional Commit title policy.

## Validation

- [ ] `bash scripts/ci-preflight.sh` (use `--base origin/<base-branch>` for a non-`main` base)
- [ ] Durable preflight hooks are installed with `bash scripts/install-git-hooks.sh`

## Maintainer checklist

- [ ] This PR is ready for review by @jatmn.
- [ ] Security-sensitive changes are called out in the summary.
- [ ] The PR should only be merged by @jatmn.
