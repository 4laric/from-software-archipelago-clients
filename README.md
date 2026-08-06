# From Software Archipelago Clients

A shared monorepo for [Archipelago] clients for From Software games. The clients
share infrastructure but ship independently. Three games are supported today:

* **Dark Souls III** — `crates/ds3-archipelago`. See [the Dark Souls III setup
  guide].
* **Sekiro** — `crates/sdt-archipelago`.
* **Elden Ring** — `crates/eldenring-archipelago` plus its pure-logic crates
  (see below).

[Archipelago]: https://archipelago.gg/
[the Dark Souls III setup guide]: https://nex-3.com/ds3/setup

## Layout

| Crate | What it is |
|---|---|
| `crates/ds3-archipelago` | Dark Souls III client DLL |
| `crates/sdt-archipelago` | Sekiro client DLL |
| `crates/eldenring-archipelago` | Elden Ring client DLL (live-game hooks) |
| `crates/er-logic` | Pure, host-tested decision logic for the ER client |
| `crates/er-codec` | Encoding helpers shared by the ER crates |
| `crates/er-semver` | Version-band parsing/ordering for the ER connect gate |
| `crates/shared` | Infrastructure shared across the game clients |

## Elden Ring

The Elden Ring client is `eldenring-archipelago`, a hooks DLL loaded into the
live game by [ModEngine3] (me3). It pairs with the `eldenring.apworld` from the
main [4laric/er-archipelago] repo — the apworld generates the seed, the DLL
plays it.

It is a **pure-runtime** client: nothing is baked into game files ahead of time.
On connect it reads the seed's slot data from the Archipelago server and
verifies a contract hash (client and apworld ship separately, so mismatched
pairs are the norm to detect, not an edge case). From there everything happens
live in the running game:

* **check detection** — location checks are detected as you pick them up and
  sent to the server;
* **item grants** — received items are granted into the live save;
* **graces** — start/bundle graces are lit as they are unlocked;
* **region locks** — regions are sealed until you receive their Region Lock
  item. Runs start from Roundtable Hold, and entering a still-locked region
  warps you back there;
* **lock hints** — you can spend progression-surface checks to reveal where a
  Region Lock is (see below).

### Lock hints

Archipelago prices a hint at a percentage of your own location count, and an
all-region Elden Ring seed carries about **4879** locations — so the default 10%
is **487 points**, roughly a tenth of the entire seed, for one hint. Elden Ring
is priced out of Archipelago's hints by arithmetic, not by a host's setting.

So the client re-denominates the host's own percentage over the ~158-location
**progression surface** — the only locations this world's own progression can
occupy, shown with a `*` in the tracker. At the Archipelago default that is
**16 surface checks per hint** instead of 487, and it still tracks the host's
`hint_cost`: a room at 5% pays less here too, and a room that made hints free
gets these free.

Your balance is on the overlay menu bar (`Lock hints: 23/16`); clicking it opens
the tracker. There you can either:

* **Hint next lock** — reveals the next lock you can actually *reach*: the one
  whose region is still sealed but whose item is already sitting somewhere
  open. You are not told which region that is until you buy it, which is the
  point — the chain order is a product of the fill, so guessing a lock by name
  means paying to find out you guessed wrong.
* **hint lock** on a specific `[locked]` region header, if you would rather aim.

Either way this publishes a **real Archipelago hint**, visible to the whole room
in the Hints tab. It is never a private reveal: a hint nobody else can see is
indistinguishable from cheating. It also never spends your Archipelago hint
points — `!hint` still shows the full total afterwards — and it can only ever
target your *own* world's Region Lock items. If a lock spilled into someone
else's world the client says so and points you at `!hint`, which is the only
tool for that case.

### Overlay hotkeys

| Key | Toggles |
| --- | --- |
| `F5` | The client overlay — main window, settings, dev console. |
| `F6` | The item tracker window. |

Both are also on the overlay's menu bar (`Hide (F5)`, `Tracker (F6)`), because a
hotkey you can only learn from a README is one you cannot find once you have
already pressed it.

They are function keys on purpose: the overlay's say box is a live text field
whenever the overlay has focus, so a letter hotkey would fight it.

Two things F5 does **not** do. It does not hide grant notices — those toasts are
the only feedback for items the game itself cannot announce, such as flask
upgrades, so hiding them would make a working feature look broken. And it does
not stop the mod: the client's logic runs off its own recurring task, not the
render loop, so a hidden overlay still reconciles, grants items and reports
checks. F5 is inert while disconnected, so the connect form can never be hidden
away.

All decision logic lives in `er-logic`, which is pure (no game, Windows, or
socket dependencies) and host-tested: `cargo test -p er-logic` runs anywhere,
including against a slot-data fixture generated by the apworld's pytest suite,
so the two sides are tested against the same data. The runtime state engine is
a reconciler that converges live game state onto the server-declared desired
state — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

[ModEngine3]: https://github.com/garyttierney/me3
[4laric/er-archipelago]: https://github.com/4laric/er-archipelago

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for building, testing, and running a
local client, and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the
Elden Ring client works.

## Other FromSoft Archipelago Clients

This isn't the only Archipelago mod for From Software games! Here are others
that we know of:

* [Armored Core](https://github.com/JustinMarshall98/Armored-Core-PSX-Archipelago)
* [Dark Souls Remastered](https://github.com/ArsonAssassin/DSAP)
* [Dark Souls II](https://github.com/WildBunnie/DarkSoulsII-Archipelago)

## License

MIT — see [MIT-LICENSE](MIT-LICENSE).
