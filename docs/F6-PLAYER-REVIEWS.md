# Opt-in F6 player reviews

Enable **Help verify locations (this session)** in F6. Review and Map buttons appear
on visible remaining locations; **Review completed locations** also exposes completed
seed locations. Restarting the client disables the option. No YAML change is needed.

A button explicitly opens the public player-review page in the default browser.
Only the AP location ID, original server location name and requested view are
included, in the URL fragment. No seed, slot, password, server address, inventory,
game position or completion claim is sent. A completion can be a boss sweep:
the reviewer must describe their own observation.

The browser checks the original name against its catalog (ignoring the optional
sweep suffix), refuses a mismatching ID, and supports locations without outdoor
coordinates. Its map is a full-catalog overview, not the live seed or a route map.
Deploy the companion world player-map page before making these controls available
in a release.

## MapForGoblins 2.1.3 inspection, 2026-09-05

Inspected the user-provided Vanilla or Randomizer 2.1.3 archive without loading
or executing its DLL:

- MapForGoblins.dll SHA-256:
  ed984d5bb3ee49e304ab02e5ac1bc1bfc3a6368c2bc8743f85edefe2a73f2ea3
- x86-64 PE; export-directory RVA and size are both zero. There are no callable
  exported functions, including the proposed MFG_AP functions.
- Bundled license permits modification and redistribution with its notices retained.
- Public source head remained a25443312dd07c21bb616bd2aeda16ee889df045,
  dated 2026-07-16, labelled v2.0.6:
  https://github.com/VirusAlex/ERR-MapForGoblins-DLL/commit/a25443312dd07c21bb616bd2aeda16ee889df045

The source already has exact hovered-row detection in src/goblin_maphover.cpp
and src/goblin_maphover.hpp, and focus/highlight/visibility facilities in
src/goblin_inject.hpp. These are internal C++ functions, not DLL exports.
This verifies a source-level implementation opportunity, not the newer DLL's ABI
or game-version compatibility. We did not disassemble/recover a callable internal
address, and did not mutate the supplied binary.

### Recommended native-pin bridge

Build an explicitly labelled source fork with a small versioned C API. Start
read-only: copy a hovered marker snapshot (original acquisition flag, live flag,
map/layer, coordinates, generation, freshness) while MFG owns and validates the
row. F6 joins the original identity to the current seed's checks and offers the
same Review action. Zero, stale or ambiguous matches show no automatic selection.
Never hand the other DLL a live game pointer.

Preserve original and rewritten live flags separately: AP and randomizer changes
can make a visibility flag differ from the catalog identity. Do not infer identity
from coordinates or icon category. An upstream marker is game-derived evidence;
it is not a second independent corroborating source.

A later highlight API can use the same generation/identity scheme without
changing completion flags. MFG remains the sole owner of its row lifecycle,
manual hides and visibility policy. See MAPFORGOBLINS-AP-API.md for the earlier
state-control proposal; the shipped client still only probes those symbols.

Before replacing 2.1.3 with such a build: compile the vanilla profile, inventory
what changed since 2.0.6, and test map open/close, reload, underground/DLC layers,
anonymous loot, live loot rewrites, manual hide, and both DLL load orders in game.
The old source should not silently replace the newer release. Waiting for the
updated source remains the lowest-maintenance option; the source fork avoids
depending on undisclosed 2.1.3 memory offsets if we proceed sooner.
