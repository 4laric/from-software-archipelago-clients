# Recovering from an auto-equip backlog

If a received-item backlog crashes the game during equipment changes, start the
game disconnected from Archipelago. Open the overlay's Console (F5 shows the
overlay) and enter:

```text
!autoequip off
```

Then connect. Items still arrive normally, but automatic equipment changes,
Physick mixing, spell memorization/backfill, and starting-loadout normalization
are disabled. The command discards queued equip requests, not inventory items.
It works before connecting and survives reconnects for the rest of the game
process. After restarting the game, enter it again before connecting.

`!autoequip` reports the current override. `!autoequip seed` restores the seed's
setting; it cannot force auto-equip on for a seed that disabled it. Weapons,
armour, talismans and tears skipped while OFF must be equipped manually. The
existing spell backfill may fill empty memory slots when SEED is restored.
No YAML edit, new seed, or save migration is needed.

Queued weapons, armour, talismans and Physick tears now share a budget of one
successful change per 500 ms. Starting-loadout normalization shares that budget.
Item-delivery pacing remains separate. Items that are not yet in inventory or
weapons held for a boss fight stay queued with their original stream metadata;
unused time does not accumulate a burst of equipment changes.

This closes the unbounded queue-drain path found while investigating Fossils's
reconnect crash report. The 500 ms spacing is a conservative mitigation, not a
measured game-engine safety threshold. Reproducing the report in game is still
required to establish whether it resolves that particular crash.

## In-game acceptance check

With auto-equip enabled, reconnect with several received armour pieces queued.
Confirm equipment changes are spaced apart, all items arrive, and the game
remains stable both in Roundtable Hold and the open world. Repeat after a load
boundary. Then restart disconnected, enter `!autoequip off`, connect and confirm
items arrive without changing equipment. Reconnect once more to verify OFF is
retained. Restore SEED and verify a newly received item equips normally.
