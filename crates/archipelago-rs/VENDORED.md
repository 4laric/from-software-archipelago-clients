# `archipelago_rs`, vendored

This crate is **not ours**. It is [nex3/archipelago_rs] vendored into the workspace at rev
`33ddae62697d2432b2d623a667e18a67e09bf567` (the crates.io `3.0.1` tree, plus the 0.6.6 Connect
version commit), MIT-licensed to Ryan Goldstein, Natalie Weizenbaum and AshIndigo. `MIT_LICENSE.md`
travels with it unchanged.

[nex3/archipelago_rs]: https://github.com/nex3/archipelago_rs

## Why it is in-tree

It used to be `archipelago_rs = { git = "https://github.com/nex3/archipelago_rs" }` — an
**unpinned** git dependency, held in place only by `Cargo.lock`. Two things made that untenable:

1. We needed to change it. A bug in `ReceivedItems` hydration could void a client's entire
   receive stream (see below), and there is no seam on our side to work around it: `Connection::new`
   takes a URL string and the crate owns its own socket, so parse, hydrate and discard all happen
   between the URL we hand in and the `Vec<Event>` we get back.
2. Upstream is not currently taking changes, so a dependency we cannot patch and cannot fork
   forward is a dependency we cannot fix bugs in.

Vendoring makes it ordinary code: reviewed here, tested by our CI, pinned by construction.

## What we changed

Kept deliberately small, so this tree stays diffable against upstream.

- `src/data/game.rs` — added `item_or_placeholder` / `location_or_placeholder` beside the existing
  `_or_err` pair. They fall back to the same synthetic `<item #N>` / `<location #N>` form the crate
  already produces for games with no data package at all.
- `src/data/located_item.rs` — `hydrate_with_games` now uses the placeholder variants, and carries
  a comment explaining why it must not fail. Plus the test module (upstream ships none).
- `Cargo.toml` — dropped the `eframe`/`egui` dev-dependencies along with `examples/`, which we do
  not build.
- Dropped `.github/` and `.gitignore`; this repo's CI and ignore rules govern.

The `_or_err` variants are **retained and still used** by `Print` and by scout-name resolution, so
the strict behaviour is still available where losing one line is the correct cost.

## The bug this fixes

`Client::handle_message` hydrates a whole `ReceivedItems` batch through one
`collect::<Result<Vec<_>, _>>`. That short-circuits, so a single item whose *sender's* location id
was absent from the sender's data package discarded **every** item in the batch. Worse, the
`index == 0` arm clears the stream *before* hydrating, so a connect-time replay that tripped this
left the client with a permanently empty receive stream — while sends kept working, because the
send path never touches this code. `Error::is_fatal()` returns `false` for `ProtocolError`, so it
surfaced as a single non-fatal log line, and `sync()` is only reachable from the index-gap branch
that runs *before* hydration. Nothing recovered; every reconnect replayed into the same failure.

Observed 2026-08-05 in an Elden Ring bundle: `location 130827133 is missing Geometry Dash's data
package`, beside `recv: stream=0 cursor=0 can_grant=true`.

**Ids are preserved; only names are synthesised.** That is what makes this safe — consumers route
on `.id()`, and the Elden Ring client additionally indexes the stream *positionally*
(`received_items().iter().enumerate()`), so dropping or reordering an entry would silently
misalign every later item against the receive cursor. A placeholder keeps the batch whole.

## Known remaining

The batch drop has two other reachable causes that this change does **not** address: the same
closure also calls `teammate_arc` (`MissingPlayer`) and `game_or_err` (`MissingGameData`), either
of which will still abort a whole batch. Both need a placeholder `Player`/`Game` design rather
than a three-line change, so they are deliberately left out of this PR.
