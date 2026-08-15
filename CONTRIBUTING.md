# Contributing

Contributions are generally welcome, but it's a good idea to chat about them in
the Archipelago Discord before writing a bunch of code. Coming up with the right
design takes time, discussion, and collaboration. We discuss active and future
games in the following channels:

* [Dark Souls III](https://discord.com/channels/731205301247803413/1005246392329052220)
* [Sekiro](https://discord.com/channels/1085716850370957462/1100511247939686481) (18+ only)
* [Elden Ring](https://discord.com/channels/731205301247803413/1114277493311033494)

## Repo shape

This is one Cargo workspace with a crate per game client (`ds3-archipelago`,
`sdt-archipelago`, `eldenring-archipelago`) plus shared crates. The Elden Ring
client is split deliberately:

* `crates/er-logic` — all the decision logic, pure Rust with no game, Windows,
  or socket dependencies. Builds and tests on any host.
* `crates/eldenring-archipelago` — the `cdylib` that hooks the live game.
  Windows-only dependencies (detours, hudhook, the game's memory layout).
* `crates/er-codec`, `crates/er-semver` — small pure support crates.

Put logic in `er-logic` whenever possible; the DLL crate should be thin I/O
glue. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Building and testing

You need an up-to-date [Rust] installation.

[Rust]: https://rust-lang.org/

**The cheap gate — run this first, on any OS:**

```
cargo test -p er-logic
```

`er-logic` is host-native, so this runs the whole ER decision core (reconciler
invariants, slot-data parsing, region locks, version gate, and more) in seconds
with no Windows machine involved. It includes an integration test against
`crates/er-logic/tests/fixtures/slot_data_fixture.json`, a fixture **generated
by the apworld's pytest** (`worlds/eldenring/tests/test_slot_data_fixture.py`
in the [4laric/er-archipelago] repo) — never hand-edit it; re-run that pytest
to refresh it. If the fixture is absent the test soft-skips with a loud line.

**The client DLLs** are Windows `cdylib`s:

```
cargo build -p eldenring-archipelago
```

builds `target/debug/eldenring_archipelago.dll` on Windows natively, or via
cross-compile (`--target x86_64-pc-windows-msvc` / `-gnu`) from another host.
The DS3 and Sekiro DLLs build the same way from their crates.

**CI** (`.github/workflows/test.yaml`) gates pushes to `main` and pull
requests on a Windows runner.

> 🛑 **That file is the source of truth for what runs, not this section.** The
> list below has drifted from it three times: `shared` and `archipelago_rs`
> were added to the test line, and a second `clippy` invocation and a release
> build were added, while this paragraph still said "all four". Each drift told
> a reader that a test ran when it did not. **When the answer matters, read the
> workflow.** Nothing currently enforces that they agree -- see the PR that
> rewrote this section for a sketch of the gate that would.

As of 2026-08-15 the job runs, in this order:

1. `cargo build` — debug, and deliberately first. Every step is fail-fast, so
   **build and test run before style**: on this repo's first ever CI run a
   `cargo fmt` failure skipped the build while three genuine compile errors sat
   on `main`.
2. `cargo test -p er-codec -p er-semver -p er-logic -p eldenring-archipelago
   -p shared -p archipelago_rs` — **six** packages, not four. `shared` and
   `archipelago_rs` each spent a period being built here but never tested,
   which means their suites ran nowhere at all.
3. `cargo fmt -- --check`
4. `cargo clippy -- -D warnings` **and**
   `cargo clippy --features=profile -- -D warnings` — **two** invocations. A
   change that is clean under default features can still fail the `profile`
   pass.
5. `cargo build --release -p eldenring-archipelago`, uploaded as a downloadable
   artifact. Scoped to that one package on purpose: a bare workspace release
   build is red under `-D warnings`.

Two things that make a local run differ from CI, and both are silent:

* **`RUSTFLAGS: -Dwarnings` is set for the whole job.** A local `cargo build`
  without it is not the same gate — warnings you never see are errors there.
* **The toolchain is `nightly`**, pinned in `rust-toolchain.toml` and installed
  explicitly by the workflow. Use nightly `rustfmt`, or `cargo fmt -- --check`
  will disagree with CI over files it has no opinion about locally.

Run the whole thing before pushing:

```
export RUSTFLAGS=-Dwarnings
cargo build
cargo test -p er-codec -p er-semver -p er-logic -p eldenring-archipelago -p shared -p archipelago_rs
cargo fmt -- --check
cargo clippy -- -D warnings
cargo clippy --features=profile -- -D warnings
```

`cargo test -p er-logic` alone remains the cheap gate to run while you work;
the block above is the one that has to pass before you push.

## Running your local Elden Ring client

The ER client is a DLL that [me3] (ModEngine3) loads into the game. To use a
local build instead of a released one, edit the me3 profile you launch with
and point its `[[natives]]` entry at your build output:

```toml
[[natives]]
path = "c:/code/er-client/target/debug/eldenring_archipelago.dll"
```

Use forward slashes — backslashes are interpreted as string escapes. Launch
the game through me3 as normal and it loads your local DLL. The same
`[[natives]]` swap works for the DS3 and Sekiro clients with their DLLs.

[me3]: https://github.com/garyttierney/me3

## Pairing with the apworld

The ER client plays seeds generated by the `eldenring.apworld` from
[4laric/er-archipelago]. Nothing is baked into game files: the client reads the
seed's slot data from the Archipelago server at connect and does everything
live. Client and apworld are kept in lockstep by two guards:

* a **version band** — the apworld's slot data declares the client versions it
  accepts, checked with `er-semver` at connect;
* a **contract hash** — both artifacts embed a hash derived from the shared
  slot-data contract (`contract_gen.rs` is auto-generated from the apworld's
  `contract.py`). A mismatch means the two were built from different contracts.

If you change the slot-data shape, change it in the apworld repo first,
regenerate `contract_gen.rs` and the pytest fixture, and bump the band. The
`cargo test -p er-logic` fixture test is what catches a silent drift.

[4laric/er-archipelago]: https://github.com/4laric/er-archipelago
