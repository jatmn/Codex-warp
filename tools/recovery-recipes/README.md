# Tag-specific recovery recipes

Recovery recipes are reviewed exceptions for an immutable release whose tagged
tool or contract can no longer produce valid artifacts. They never authorize a
new version, source SHA, tag target, release identity, or publication history.

Name a recipe `official-<tag>.json` or `nightly-<tag>.json` and validate it
against `schema.json`. Record both the tagged inputs and every replacement,
including the exact inventory difference, rationale, expiry date, and removal
condition. A manifest-producing replacement must identify an immutable tool
archive and an immutable upstream schema URL. Vendor that schema as
`schemas/<sha256>.json`; the filename, declared digest, and file bytes must
match. Mutable or implicit schema fallbacks are forbidden.

Recipes must be merged to protected `main` before dispatching recovery. Remove
an expired recipe through normal review after its release is published and its
retained evidence is verified.
