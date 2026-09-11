# 0.6.0.8: resume 0.5.x seeds

The client accepts the audited contracts for all existing 0.5.x versions:
0.5.0 through 0.5.8. Existing asyncs do not need regeneration, an APWorld update
or a save migration to use this bridge. Unknown versions and mismatched hashes
retain the existing warning.

The old seed's checks, regions, item mappings and sweep membership remain in use.
New options absent from the seed retain their existing defaults. This does not
retrofit generation or logic fixes into an old multiworld. The game executable
compatibility gate remains separate.

Eighteen independent default/rich fixtures cover all nine versions; see
crates/er-logic/tests/fixtures/legacy_05/README.md for provenance and reproduction.
Live in-game testing remains outstanding.
