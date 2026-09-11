# 0.6.0.8: resume older 0.5.1 async seeds

The client now recognizes seeds identifying themselves as
`apworld/0.5.1 contract/13db0b3a` as compatible. This lets a player use the current
client's executable support without regenerating their existing async.
No APWorld update, save migration, or new seed is required. Other version/hash
pairs retain their existing checks.

The bridge uses the older seed's check IDs, region mappings and sweep membership.
It does not retrofit fixes to server-side generation or logic. Two generated
0.5.1 fixtures exercise the current Windows parsers; live in-game verification
is still outstanding. Fixture provenance is in
`crates/er-logic/tests/fixtures/legacy_051/README.md`.
