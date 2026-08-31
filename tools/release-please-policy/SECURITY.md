# Release Policy Dependency Review

This package is isolated release-policy tooling. Production application code
does not depend on it.

## yargs 17.7.3 review

Socket reported the transitive `release-please -> yargs@17.7.3` dependency as
likely obfuscated. The flagged `build/index.cjs` is the package's documented
Rollup-generated CommonJS bundle, not hidden source. Review on 2026-08-30 found:

- npm integrity
  `sha512-GZtjxm/J/4TSxuL3FNYjCmLktBTnIw/rVmKSIyKeYAZpmJB2ig9VauCC5xsa82GNKVKDAqpOn3KVzNt0zmrU0g==`
  matches this lockfile;
- npm `gitHead` `2f7df4db9630f8d1e4c1d7cbae8e69753cb79185` matches
  the official upstream `v17.7.3` tag;
- the upstream release contains one entry-point compatibility fix; and
- npm reports a registry signature for the published package.

`release-please` is intentionally a dev dependency used only by the pinned
policy harness. Release, nightly, and recovery workflows install with
`npm ci --omit=dev --ignore-scripts`, so neither Release Please nor yargs is
installed in credential-adjacent jobs. The all-dependencies harness also uses
`--ignore-scripts` and runs with read-only repository credentials and no
protected environment secret.

The exact yargs version and integrity are asserted by the offline policy tests.
Changing either requires a new source and supply-chain review.

## AJV advisory

The same review found GitHub advisory `GHSA-2g4f-4pwh-qvx6` against the former
AJV 8.17.1 pin. These validators never enable AJV's affected `$data` option,
but the direct dependency was upgraded to patched AJV 8.20.0. A clean npm audit
is required as rollout evidence whenever this lockfile changes.
