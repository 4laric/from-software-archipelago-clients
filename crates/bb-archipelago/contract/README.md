# Vendored native-grant runtime contract

`bb-native-grant-contract.v5.json` is a **verbatim copy** of
`research/runtime/bb-native-grant-contract.v5.json` in the
[`4laric/bb-archipelago`](https://github.com/4laric/bb-archipelago) world/tooling
repo, which is its single source of truth. It is the machine-readable statement
of what the `bb-native-grant-v5` harness does to the guest process: hook-site
RVAs, native-routine RVAs, the state-cell layout, the descriptor formula, the
image-assert byte strings, the relocatable payload blobs, and the fail-closed
policy rows.

**Do not hand-edit this file.** `src/native/contract.rs` parses it at build/run
time so that no address in the Rust client is a hand-copied number — exactly the
constant-duplication hazard RESEARCH-BASELINE.md called out. When the contract
changes upstream, re-copy the file from the world repo (same way
`crates/er-logic/tests/fixtures/slot_data_fixture.json` is refreshed from the
apworld's pytest) and let the parser and its cross-check tests re-derive
everything.

`src/native/contract.rs` additionally asserts that the vendored contract's
`build` / `harness` / `bridge_protocol` still equal the crate's
`RUNTIME_BUILD` / `HARNESS_VERSION` / `BRIDGE_PROTOCOL` constants, so a drift
between this copy and the code that consumes it fails a unit test rather than
shipping.
