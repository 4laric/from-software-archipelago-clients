# Bobler playtest: three probes retired or narrowed (2026-08-17)

Source: `archipelago-2026-08-17.log`, supplied by Bobler/Alaric. The log itself is not committed.

## Enemy scaling: repeated writes are not a recompute primitive

At `13:50:14`, unloaded `npc_param 49801020` still had `6577` max HP immediately after its rung
changed from `7080` to `7010`. At `13:51:13` it loaded as a boss carrying `7010` at `2792/2792`.
The vanilla inputs make both readings exact (integer rounding aside):

- `NpcParam.hp 2447 * SpEffect 7080 maxHpRate 2.688 = 6577`
- `NpcParam.hp 2447 * SpEffect 7010 maxHpRate 1.141 = 2792`

There was no reapplication between those readings. Loading reconstructs max HP from the rung that
was accepted while unloaded. Conversely, three loaded entities remained unchanged after three
remove/re-apply cycles at `13:50:18`; `ready` is not sufficient and repeating the same write does
not force a recompute. The client now observes and reports stale loaded writes without churning them.

## AP item scout: data path proven, production cache retained

At `13:48:48`, the server returned all `1760/1760` requested locations with item and owner data.
The downstream shop pass later reported `0 no scout entry`. The cache and its failure telemetry are
production infrastructure; the per-location proof dump is retired in favour of a one-line count.

## AP flower: runtime census closed, visual experiment remains

The one-shot probe found `oo2core_6_win64.dll` loaded, so KRAK decompression is callable in-process.
It also confirmed a 25,600-byte block-aligned mip-0 splice versus the shipped 8,388,608-byte atlas.
Mip 1 begins at `(1066, 574)`, so no mip below 0 is independently block-aligned.

That runtime census cannot answer the remaining questions and is retired. The next evidence must be
visual: test a non-KRAK DCX in the loader, and test a mip-0-only flower at every UI scale. The log
only saw a top-level `menu/` directory and explicitly marked the installed sprite sheet unverified.
