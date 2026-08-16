## Summary

-

## Contribution checklist

- [ ] I checked for duplicate or overlapping existing pull requests.
- [ ] This PR does not introduce a new implementation language.
- [ ] This PR does not add Python for any reason.
- [ ] I can respond to review feedback within one week.

## Validation

- [ ] `bash scripts/source-checks.sh` (fmt, typos, docs prose, JS, crate-wide clippy)
- [ ] `cargo update --workspace --locked` (CI also runs this)
- [ ] `cargo test --locked`
- [ ] `cargo build --locked`
- [ ] `target/debug/codex-warp --version`
- [ ] `target/debug/codex-warp --help`

## Maintainer checklist

- [ ] This PR is ready for review by @jatmn.
- [ ] Security-sensitive changes are called out in the summary.
- [ ] The PR should only be merged by @jatmn.
