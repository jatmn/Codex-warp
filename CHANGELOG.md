# Changelog

## [0.1.2](https://github.com/jatmn/Codex-warp/compare/v0.1.1...v0.1.2) (2026-09-15)


### Bug Fixes

* **release:** peel nightly tags with the job token after App create ([#131](https://github.com/jatmn/Codex-warp/issues/131)) ([db2e64d](https://github.com/jatmn/Codex-warp/commit/db2e64dabc516ba539b87592cca0f87baa3ead87))
* **release:** recover nightly tags when the receipt is missing ([#129](https://github.com/jatmn/Codex-warp/issues/129)) ([7291966](https://github.com/jatmn/Codex-warp/commit/729196696ac847c95e5536a7cb03db4213da9efa))

## [0.1.1](https://github.com/jatmn/Codex-warp/compare/v0.1.0...v0.1.1) (2026-09-13)


### Bug Fixes

* **continue_guard:** keep long multi-agent sessions auto-continuing ([#118](https://github.com/jatmn/Codex-warp/issues/118)) ([1a39847](https://github.com/jatmn/Codex-warp/commit/1a39847fd716a60f6d7cce2ce72fcfc8c0f0e862))
* **models:** honor WebUI disable for colliding cross-provider slugs ([#117](https://github.com/jatmn/Codex-warp/issues/117)) ([7f149d4](https://github.com/jatmn/Codex-warp/commit/7f149d4c24259fa446a7e65fab1530beab91c217))
* **provider:** identify OpenCode Go sessions ([#121](https://github.com/jatmn/Codex-warp/issues/121)) ([d81deaa](https://github.com/jatmn/Codex-warp/commit/d81deaad5b307db3009d9f6560be29a92d92d77a))
* **release:** classify App-token 404s when creating nightly tags ([#128](https://github.com/jatmn/Codex-warp/issues/128)) ([478d974](https://github.com/jatmn/Codex-warp/commit/478d974c96c95a820b61f749e43064d6603ec882))
* **release:** include maintenance notes without triggering releases ([#125](https://github.com/jatmn/Codex-warp/issues/125)) ([e837600](https://github.com/jatmn/Codex-warp/commit/e837600e9576080527d45ffb2681e5c6bb356ad8))
* **security:** confine docs checker path.resolve to the repo base ([#120](https://github.com/jatmn/Codex-warp/issues/120)) ([12de5b1](https://github.com/jatmn/Codex-warp/commit/12de5b1598bea7f62b6d79021f959fb08a92325c))
* **security:** harden documentation path confinement ([#115](https://github.com/jatmn/Codex-warp/issues/115)) ([26431ce](https://github.com/jatmn/Codex-warp/commit/26431cefd421e3c31c07382c8da3741cf578ae2e))


### Documentation

* document live releases and commit title templates ([#114](https://github.com/jatmn/Codex-warp/issues/114)) ([9a0794c](https://github.com/jatmn/Codex-warp/commit/9a0794ce93acc5c9022e192f52999271a72413f1))


### Build System

* **deps:** Bump crate-ci/typos from 1.49.0 to 1.50.1 ([#124](https://github.com/jatmn/Codex-warp/issues/124)) ([fdef744](https://github.com/jatmn/Codex-warp/commit/fdef744542eeeb80a189929d8f2d0a0e43f9d34f))
* **deps:** Bump reqwest from 0.13.4 to 0.13.5 ([#126](https://github.com/jatmn/Codex-warp/issues/126)) ([1ca3cdb](https://github.com/jatmn/Codex-warp/commit/1ca3cdbee5b97e8a0a6859cc7ed64f5c6cfbd9fb))
* **deps:** Bump taiki-e/install-action from 2.86.8 to 2.87.4 ([#123](https://github.com/jatmn/Codex-warp/issues/123)) ([010133c](https://github.com/jatmn/Codex-warp/commit/010133c914229c2688877a216a89c1052ffd7b39))
* **deps:** Bump taiki-e/install-action from 2.87.4 to 2.87.9 ([#127](https://github.com/jatmn/Codex-warp/issues/127)) ([653a113](https://github.com/jatmn/Codex-warp/commit/653a113ca22799837c4fe6739903b72850e0cbb5))
* **deps:** Bump toml from 1.1.4+spec-1.1.0 to 1.1.5+spec-1.1.0 ([#122](https://github.com/jatmn/Codex-warp/issues/122)) ([f539391](https://github.com/jatmn/Codex-warp/commit/f539391f9b332f4819095ece411440ece7489ce6))


### Continuous Integration

* stop scanners treating the GitHub App action pin as a secret ([#119](https://github.com/jatmn/Codex-warp/issues/119)) ([783bbda](https://github.com/jatmn/Codex-warp/commit/783bbda9986609b7d51c65d3b22bed165a2df4f2))

## [0.1.0](https://github.com/jatmn/Codex-warp/compare/v0.0.1...v0.1.0) (2026-09-02)


### Bug Fixes

* **release:** port sandbox-proven draft and recovery machinery ([#111](https://github.com/jatmn/Codex-warp/issues/111)) ([db2aeef](https://github.com/jatmn/Codex-warp/commit/db2aeefa6bc123e538f662965377d79a300fcba3))


### Build System

* **release:** automate official and nightly releases ([#110](https://github.com/jatmn/Codex-warp/issues/110)) ([a73617d](https://github.com/jatmn/Codex-warp/commit/a73617ddbacd43a8cc19c4f48dc60866eb6e9918))

## Changelog

Codex Warp release notes are maintained by Release Please from reviewed
Conventional Commit titles. The project began release automation at version
0.0.1; the first public release is planned as 0.1.0.

For changes before the first automated release, see the repository history.
