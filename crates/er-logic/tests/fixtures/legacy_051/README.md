# 0.5.1 compatibility fixtures

Generated from world tag v0.5.1 (1aad90c3), contract 13db0b3a, using AP 0.6.7
and fixed seed 1706. These are complete slot-data snapshots from WorldTestBase,
not completed multiplayer fills or a recording of the reporting player's async.
Do not hand-edit the JSON.

To reproduce, install the unmodified v0.5.1 world into an isolated AP checkout,
then run from that AP directory with AP_NONINTERACTIVE=1 and
SKIP_REQUIREMENTS_UPDATE=1:

```python
import json
from pathlib import Path
from worlds.eldenring.tests.test_gf_slot_data_fixture import SlotDataFixtureDefault, SlotDataFixtureRich
for cls in [SlotDataFixtureDefault, SlotDataFixtureRich]:
    test = cls()
    test.world_setup(seed=1706)
    sd = test.world.fill_slot_data()
    Path(cls.__name__ + ".json").write_text(json.dumps(sd, sort_keys=True), encoding="utf-8")
```

The classes own the exact options. Both keep all regions and emit 4,948 locations,
29 region locks and 116 area-lock rows. Rich additionally exercises grace
attunement and the scaling ceiling. The current Windows parser test preserves
these counts and verifies the profile fallback and required feature support.

The contract adds only profile, lockHintPlacements and mineMaterialRoll since
0.5.1; profile already has a legacy selector, and the other two are optional.
New DeathLink amnesty values default to 1/1; rune reward scaling and boss-name
reveal default off. dungeonSweeps was retagged Bedrock-only, but these old seeds
emit an empty object; the live sweep wire remains dungeonSweepFlags.

Old location IDs, regions and sweeps must remain seed-provided. This bridge does
not repair generation bugs in an old seed. Live resume/check/shop/sweep/reconnect
smoke testing on an updated executable remains required before claiming runtime
verification. The game executable gate remains independent of this seed gate.

0.5.1 and 0.5.3 have identical declared contract keys, shapes, requiredness, profiles,
and option subkeys. These independent 0.5.1 snapshots preserve the older corpus.
