# Bloodborne Archipelago client

This crate is the standalone Bloodborne client path. Unlike the native Dark
Souls III and Sekiro DLLs, it reads Bloodborne location flags directly from
shadPS4 process memory and uses a shadPS4/Cheat Engine file bridge for item
delivery while the emulator integration is being developed.

The first bridge contract supports received-item grants:

- The client writes one atomic `GRANT` command only when no command is pending.
- The runtime validates the expected current quantity before changing anything.
- Existing stacks use a guarded record update; absent items use the native
  game-thread `ItemGrant` path.
- The runtime clears the command only after verifying completion.
- The client advances its durable received-item watermark only after observing
  a terminal success state and an absent command file.

The current bridge contract is `BBGRANT1` with harness
`bb-native-grant-v5`. Both tokens must appear in the harness state before the
client publishes a command. A terminal harness failure blocks that AP item with
a bounded diagnostic while location polling and the server connection continue.

Item-ID mappings and location checks are deliberately not hard-coded here. The
local rows are only a migration/test fallback. On connect, the apworld's
`runtime_locations` and `runtime_items` slot-data tables replace them. Every
runtime item carries an explicit raw descriptor, normalized id, item category,
descriptor-evidence class, quantity and receive policy. The client never derives
an equipment descriptor from an ItemLot id. Malformed or unvalidated present
tables fail closed before a bridge command can be published.

The crate also wires pure policy for auto-equip and auto-upgrade into the
ordered receive loop behind mockable backend operations. Equipment
slots are selected from AP receive-stream ordinals rather than the live loadout,
so reconnect replay converges. Right- and left-hand weapons rotate over their
two slots, Caryll Runes rotate over three slots after the Rune Workshop Tool has
appeared in the feed, attire uses its fixed body slot, and Oath Runes use their
dedicated slot. Upgrade targeting is raise-only and clamped to +10. A durable
pending plan records the selected level, target slot, and completed stages
before the receive watermark advances, so a restart between grant and equip
does not re-grant. The live bridge accepts allowlisted category-0 equipment such
as the clean-save Saw Spear canary. Live auto-equip and reinforcement mutation
remain disarmed until their separate v0.18 memory contracts are validated.

## Standalone client

`bb-ap-client` connects as game `Bloodborne`, requests a full item sync, polls
configured location flags when its safety context is valid, and grants received
goods strictly by AP index. A seed-and-slot keyed JSON ledger durably binds the
validated save identity before planning any grant and is saved after each
verified stage, so reconnects and process reloads skip acknowledged indices.
Location reads and item mutation share the same gameplay/save gate; losing or
changing that context cannot start or acknowledge a delivery.

The seed also names one `goal_location`. After that location survives the same
debounced flag read as every other check, the client sends Archipelago goal
status before acknowledging the location. On reconnect it re-sends goal status
when the server already records that location, so a dropped connection cannot
strand a completed Father Gascoigne run.

```text
bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock]
```

Use `runtime-config.example.json` as the configuration shape. Its zero AP IDs
are placeholders to replace with IDs emitted by the APWorld. The test location
binds the statically confirmed Pebble acquisition flag `52100000`. On Windows,
live mode discovers shadPS4, reads the launch-specific eboot base from
`shad_log`, verifies the Bloodborne 01.09 setter signature, and resolves flags
through the manager's group tree. The diagnostic reader is live, but automatic
`LocationChecks` are fail-closed because the live backend cannot yet prove a
loaded-save identity or a stable gameplay state. Mock mode supplies both,
requires the configured identity, and debounces true reads before reporting.
Run the client as administrator when shadPS4
is elevated. `--mock` applies `mock_set_flags` and exercises the complete
network, check, ordered-grant, acknowledgement, and persistence loop without
game-memory access.

The read-only diagnostic below prints whether a decimal event flag is set:

```text
bb-flag-probe SHAD_LOG EVENT_FLAG [OUTPUT]
```

## BBLauncher companion startup

`tools/start-bloodborne-ap.ps1` requests elevation once, starts BBLauncher,
waits while the player selects the build/patches and presses Play, then starts
`bb-ap-client` at the same privilege level. This is a companion rather than a
BBLauncher binary patch; the current launcher settings format has no supported
external-command field.

The live backend automatically reattaches after a stale shad process handle. If
shad is temporarily unavailable, location polling stays connected to AP, reports
the failure at a bounded cadence, and announces recovery after the new process is
readable.

Seeds that claim any vanilla award is suppressed also carry the exact
suppression-plan SHA-256 and manifest format. Configure `suppression_manifest`
to the build manifest and `installed_gameparam` to the binder actually loaded by
the game. The client requires the installation-relative `param/gameparam` path,
independently hashes that installed file, and refuses the seed if it does not
equal the manifest output; the separate build artifact is explicitly rejected
as installation evidence.

Runtime event flags, normalized item IDs, bridge paths, and future executable
signatures stay in this client-side configuration/backend layer. They are never
read from Bloodborne world-design data.
