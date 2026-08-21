# RELEASE CONTINGENCY — Tarnished Edition (game update 2026-08-28)

What to do when Elden Ring bumps its executable version and every updated player hits the
version gate at once. Read this BEFORE the update lands; the gap window is measured in hours of
Discord traffic, not days of calm.

## 1. What the player sees (already shipped)

`game_version_gate::check()` runs first in `DllMain`, before anything can touch an RVA
(`crates/eldenring-archipelago/src/lib.rs`, `crates/eldenring-archipelago/src/game_version_gate.rs`,
wording in `crates/er-logic/src/game_version.rs`, host-tested). On an unsupported executable the
player gets ONE message box naming the detected version and the two supported ones, covering both
directions (game older / game newer than the client), and the game then boots vanilla — no
backtrace, no write to any save. This replaced the raw `Rust panic: Unsupported game version
<x>` over twenty frames of `DllMain` (Duskerno, Nexus, 2026-08-15; clients#233).

So on 08-28 the failure mode is already survivable. What remains is the support load and the
turnaround to a compatible build. That is what this file is for.

## 2. The dependency chain — where new RVAs come from

The version-to-address table is NOT ours. It lives in the third-party `eldenring` crate
(`vswarte/fromsoftware-rs`, `crates/eldenring/src/rva.rs`): an `ERGameVersion` enum with one arm
per supported executable, each arm returning a full `RvaBundle` (`rva_ww::RVAS`,
`rva_jp::RVAS`). `rva::get()` panics when the running executable matches no arm — by design,
loudly, upstream.

This client pins that crate by git rev in the workspace `Cargo.toml` (section
`[workspace.dependencies]`, `fromsoftware-shared` / the `eldenring` crate it re-exports). As of
this writing the pin is rev `8c67a84f394bd58efdb06a5be47f988e0c1a9995`, whose table covers
exactly:

- Worldwide (EN) `2.6.2.0`
- Japanese (JP) `2.6.2.1`

When the game updates:

1. **Upstream (vswarte) — or a contributor PR to that repo — derives the new executable's RVA
   bundle** (pattern/offset work against the new `eldenring.exe`) and adds a new
   `ERGameVersion` arm plus its `rva_<lang>` table. We cannot do this from a changelog; it needs
   the new binary.
2. **We bump the pin** to the rev carrying the new arm. A pin bump is a RELEASE decision (it
   force-couples the client half of every in-flight world change) — the maintainer bumps it
   deliberately, never inside a feature PR.
3. **We extend the gate**: `game_version_gate.rs::Supported::from_lang_version` and
   `er_logic::game_version::{REQUIRED_WW, REQUIRED_JP}` must gain the new version IN THE SAME
   change as the pin bump. 🛑 The gate re-implements upstream's detect because `rva::get()`
   panics instead of erroring; if the gate's arms and the crate's arms drift, the gate passes an
   executable the RVA table then panics on — worse than no gate, because the player was told it
   was fine first. The 🛑 at the top of `game_version_gate.rs` is the standing warning; this
   paragraph is the checklist behind it.

## 3. Verifying WITHOUT a build (the source-read checklist)

You can confirm most of a compatibility claim without compiling or running anything:

1. **Read the pinned crate source.** After any `cargo build`/`fetch`, the exact pinned tree is at
   `$CARGO_HOME/git/checkouts/fromsoftware-rs-*/<rev>/crates/eldenring/src/rva.rs`. Get the rev
   from `Cargo.lock` (`name = "eldenring"`, the `source = "git+...?rev=<rev>"` line) — never
   guess it; more than one rev can sit in `checkouts/`.
2. **The new version arm exists** in `ERGameVersion::from_lang_version` (lang id AND version
   string both — the JP build is a different version number, not a language toggle).
3. **The arm's bundle is complete**: the arm must return a full `RvaBundle` table for the new
   version, not a subset. A partial table compiles fine and produces wrong addresses at runtime.
4. **Lockstep**: `Supported::from_lang_version` arms == `ERGameVersion` arms, and
   `REQUIRED_WW` / `REQUIRED_JP` == the same two (or more) version strings. This is a
   line-by-line diff you can do by eye in two minutes.
5. **CI compiles it**: a pin-bump PR gets the full Windows build + test + fmt + clippy gate for
   free. A green run proves the table typechecks against everything we call — it says NOTHING
   about whether the addresses are right.

## 4. The live smoke test (the only full verify)

Compilation proves the table exists; only the real executable proves the addresses. On the first
client build carrying the new RVAs, ONE volunteer (or the maintainer) on the updated game:

- launches with the client installed — the gate must NOT fire;
- connects to any room — the client log's connect banner self-identifies the build and the
  slot_data contract version (grep `SESSION START`; logs append across launches);
- performs one known check (pick up any randomized item) and sees it report;
- receives one item and sees it arrive.

If any step fails, roll the pin back and re-open the gap window. Do NOT ship to everyone on a
source-read alone.

## 5. The Discord message for the gap (ready to paste)

Post when the game update lands and the client does not yet support it. Keep it ASCII; it should
read like the in-client screen so players recognize the same wording.

```
**Elden Ring updated -- Archipelago client not yet compatible**

The game just updated past the version this client supports (Worldwide 2.6.2.0 / Japanese
2.6.2.1). If you update, the client will show "unsupported game version" and switch itself off
for that launch. Your save is NOT touched and the game runs normally -- you just won't send or
receive Archipelago checks until a matching client build is out.

- If you have NOT updated and want to keep playing Archipelago: hold off on the update for now.
- If you HAVE updated: your Archipelago save is safe. Wait for the new client build here, or
  downgrade the game (Steam: depot rollback) if you can't wait.

We're waiting on the upstream address table for the new executable, then a client build follows.
No ETA promises in channel -- we'll post the moment a compatible build is out.
```

Adjust the two version strings to whatever the gate reports at the time (they live in
`er_logic::game_version::{REQUIRED_WW, REQUIRED_JP}`).

## 6. What NOT to do

- **Do not disable or soften the gate** to get past a bad week. The gate is the only thing
  standing between a player and a panic mid-`DllMain`; a client that "runs anyway" on unknown
  addresses corrupts saves, which is strictly worse than a locked-out week.
- **Do not bump the pin blind.** A pin bump without the §3 source-read is how a partial table or
  a gate drift ships. The bump, the gate arms, and the `REQUIRED_*` constants are ONE change.
- **Do not touch Cargo refs in a feature PR** (standing handoff rule): the pin is coupled to the
  release, not to any lane.
- **Do not promise an ETA in the Discord message.** The bottleneck is upstream's table, which we
  do not control.

---

Facts verified 2026-08-21 against the pinned rev `8c67a84` source: two `ERGameVersion` arms
(`(EN, 2.6.2.0)`, `(JP, 2.6.2.1)`) matching the gate's `Supported` arms; `rva::get()` panics via
`unwrap_or_else(|e| panic!(...))` on detect failure. Re-verify §2's pin and arms whenever you
read this — they move with each game update.
