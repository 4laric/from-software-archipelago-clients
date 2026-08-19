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
`bb-native-grant-v3`. Both tokens must appear in the harness state before the
client publishes a command. A terminal harness failure blocks that AP item with
a bounded diagnostic while location polling and the server connection continue.

Item-ID mappings and location checks are deliberately not hard-coded here yet.
The local rows are only a migration/test fallback. On connect, the apworld's
`runtime_locations` and `runtime_items` slot-data tables replace them. Malformed
present tables fail closed rather than mixing two seed contracts.

The crate also contains pure policy for auto-equip and auto-upgrade. Equipment
slots are selected from AP receive-stream ordinals rather than the live loadout,
so reconnect replay converges. Right- and left-hand weapons rotate over their
two slots, Caryll Runes rotate over three slots after the Rune Workshop Tool has
appeared in the feed, attire uses its fixed body slot, and Oath Runes use their
dedicated slot. Upgrade targeting is raise-only and clamped to +10. Runtime
inventory-instance and equipment writes remain Bloodborne-specific follow-up
work.

## Standalone client

`bb-ap-client` connects as game `Bloodborne`, requests a full item sync, polls
configured location flags, sends new location checks, and grants received goods
strictly by AP index. A seed-and-slot keyed JSON ledger is saved after each
verified grant, so reconnects and process reloads skip acknowledged indices.

```text
bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock]
```

Use `runtime-config.example.json` as the configuration shape. Its zero AP IDs
are placeholders to replace with IDs emitted by the APWorld. The test location
binds the statically confirmed Pebble acquisition flag `52100000`. On Windows,
live mode discovers shadPS4, reads the launch-specific eboot base from
`shad_log`, verifies the Bloodborne 01.09 setter signature, and resolves flags
through the manager's group tree. Run the client as administrator when shadPS4
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

Runtime event flags, normalized item IDs, bridge paths, and future executable
signatures stay in this client-side configuration/backend layer. They are never
read from Bloodborne world-design data.
