# 0.6.0.8: bind receive cursors only to loaded characters

GameMan can remain available at the main menu with save_slot=-1. The client
previously latched that sentinel as a real character and resumed its saved cursor,
even after the actual character loaded. Poppy's report recorded cursor 1081 for
slot -1 while the real slot 1 and its reconciler ledger remained at 803.

Character coordinates now require an in-world player, captured inventory and a
valid save slot (0-9). This prevents early binding and loading-time play-time
stamps. Existing JSON remains readable; no cursor deletion or reset is necessary.
On restart the real character's cursor is selected and existing reconciliation
rules handle outstanding deliveries. No watermark is forcibly rewound.

The separate inventory talk gate may still defer goods delivery. The supplied log
shows it closing without reopening; this change does not bypass that protection
or claim all 278 stream entries were missing. In-game verification remains needed.
