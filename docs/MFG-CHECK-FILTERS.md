# Map check filters

The source-built map engine owns the check-only, progression-only and in-logic-only filters. The client shares a complete leased snapshot when any session map workflow is enabled: follow pins, color pins, or the separate check-state sharing checkbox. Color selection is not required.

The complete snapshot contains every qualified map/enemy lot belonging to current-seed checks with exact server names matching the pinned catalog. Neutral and checked checks are included. No hidden item content is read or scouted. Progression means this seed's progression surface, not the class of its actual randomized item.

In logic uses the existing tracker region-access definition: a known empty coarse region is ungated; other known regions require membership in the currently open coarse-region set. Unlike the permissive tracker fallback, an unknown location/region mapping does not assert in-logic status. Additional quest, key, puzzle and fine-location conditions are not evaluated. The map UI describes this limitation.

Shared lots union check membership, progression eligibility and reachability independently. Bit 8 separately records that at least one same check has both progression and in-logic status; enabling both filters must require bit 8, not merely bits 2 and 4 from different checks.

Transport: capability 4, ABI 1, export MFG_AP_SET_CHECK_STATES_V1, natural C struct of three u32 fields (lot_table, lot_row, flags). Flags CHECK=1, PROGRESSION=2, IN_LOGIC=4, PROGRESSION_IN_LOGIC=8. Full snapshot every 1 second with a 3-second lease; empty snapshot with a positive lease means active with no matched checks. Count zero and lease zero clears immediately. Existing hover and style APIs remain unchanged. Disconnect, seed reset, world transition and disabling the last map workflow clear the snapshot; interrupted updates expire at the engine.

Validation: host-native er-logic tests cover neutral/current-seed membership, stale catalog names, shared-lot conjunction and unknown/open-region handling. Windows compile checks validate the thin optional DLL transport separately; no game runtime test is implied by these checks.


Map progression targets now exclude enabled sweep members from the original progression surface and include each identified granting boss when at least one member was on that surface. The same target set feeds orange rings, progression scaling and progression-only filtering. F6 stars and F5 [P] preserve the original seed placement surface. Boss identities join seed locationFlags and boss metadata through the committed world datamines BOSS_REWARD_DEFEAT and BOSS_DROP_ENTITY. Acquisition flags are not defeat flags: Godrick 510010 maps to 10000800, and Tree Sentinel 530100 maps to 1042360800. The reproducible tools/export_mfg_boss_flags.py exporter records both input hashes. All seed-valid locations participate, including field bosses absent from bossLocations. An unresolved group excludes member targets and logs once without inventing a boss. F10 no longer fires the client stamina probe, leaving it available to map settings.


Native boss icons use immutable defeat-flag identity because they carry no item lot. New capability MFG_AP_CAP_BOSS_CHECK_STATES_V1 (8) permits kind MFG_AP_BOSS_DEFEAT_FLAG (3) in the check-state entry lot_table field, with lot_row holding the exact event flag. Hover and old style exports retain MAP/ENEMY only. The client publishes all identified current-seed boss checks (not only progression targets); flags retain same-check conjunction. Older engines receive ordinary lot states and a visible update notice instead of unsupported kind 3 entries. Boss states use authoritative seed identity, not a guessed item or display name.

Enabled native sweep triggers with no individual boss AP check represent their remaining seed-valid member checks. Their progression and region-access bits use member witnesses, with the conjunction requiring the same member. Once every member is checked, that trigger state disappears. A trigger can only highlight an existing native map marker; the actual Scarab fixture uses flag 34100800, which is absent from the current vanilla native pin reference.
