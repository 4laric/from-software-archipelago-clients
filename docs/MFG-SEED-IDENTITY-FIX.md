# Seed identity fix

The map check-state publisher now joins baked `(lot table, lot row, acquisition
flag)` records to the active seed's `locationFlags`. Reachability and progression
are evaluated on those seed AP IDs. Baked AP IDs and display names no longer
gate publication of these states.

For example, the older baked map registry assigns AP ID 7771401 to acquisition
flag 13007000 / map lot 13000000. The current world assigns 7771400 to that same
acquisition and 7771401 to a different Farum reward. The old publisher rejects
the renamed/reassigned candidate and cannot publish its reachable state even
when Farum's region flag is open.

The seed mapping is captured before whetblade poll-flag rewrites, and cleared on
seed changes. Only IDs present in the active server check sets participate.
Shared lots still require the same check to witness progression AND reachability.
No region is unlocked by this change; missing mappings remain unknown.

Install the updated `eldenring_archipelago.dll` with the paired map package.
Keep `in_logic_only=1`. Existing seeds do not need regeneration. The separate
hover review/name checks are unchanged.

Validation: Windows Release client build; 1495 er-logic tests; er-logic clippy
with warnings denied; workspace formatting. Regression tests cover the Farum
ID shift with its region open and closed, absent seed IDs, and split witnesses.
