# 0.5.x compatibility fixtures

Independent default and rich slot-data snapshots for every 0.5.0 through 0.5.8
version, generated from the source commits in manifest.json with AP 0.6.7 and
WorldTestBase seed 1706. These are slot-data snapshots, not completed multiplayer
fills or player recordings. Do not edit the JSON by hand.

| Version | Source commit | Contract | Locations per fixture |
|---|---|---|---|
| 0.5.0 | 3320d231e9eb4db0d3335a80b8061ead67ea6f3a | contract/13db0b3a | 5048 |
| 0.5.1 | 1aad90c3ed687a72851ee75397476e8af3a7fb1f | contract/13db0b3a | 4948 |
| 0.5.2 | 31c69b64ad7c71284bdd793544412454228df5ea | contract/13db0b3a | 4915 |
| 0.5.3 | fd011f3be9ff2b558187216612ea0b2250bd4fdb | contract/13db0b3a | 4909 |
| 0.5.4 | 1651f2dcd2166204558a9367bd628e51ee1f6f80 | contract/13db0b3a | 4909 |
| 0.5.5 | 8d0fd377a6a3d9840be44a27977e2116556ef16e | contract/8397a952 | 4909 |
| 0.5.6 | 44d42d1bf2f842c10230f01c0b56dc1d95f32665 | contract/8397a952 | 4909 |
| 0.5.7 | 52bf6f8c0df98bd05fafa9b14a69392b6beae1aa | contract/ffc0f1b5 | 4911 |
| 0.5.8 | 4f39a69e9fdd3243d707117508fbf129c6352b43 | contract/ffc0f1b5 | 4911 |

0.5.4, 0.5.6 and 0.5.8 have no release tag in the fetched world repository;
the pinned commits are mainline snapshots immediately before the next version
transition. The bridge enumerates exact version/hash pairs, never arbitrary
future 0.5.x versions or different hashes.

To reproduce each pair, check out its commit into an isolated world tree and
install it using tools/gf_test.py --ap-dir PATH --install-only. From that AP
checkout, with AP_NONINTERACTIVE=1 and SKIP_REQUIREMENTS_UPDATE=1:

```python
import json
from pathlib import Path
from worlds.eldenring.tests.test_gf_slot_data_fixture import SlotDataFixtureDefault, SlotDataFixtureRich
for cls in [SlotDataFixtureDefault, SlotDataFixtureRich]:
    test = cls()
    test.world_setup(seed=1706)
    sd = test.world.fill_slot_data()
    version = sd["versions"].split()[0].split("/")[1]
    Path(version + "_" + cls.__name__ + ".json").write_text(json.dumps(sd, sort_keys=True), encoding="utf-8")
```

The original classes specify the options. All fixtures retain 29 region locks and
116 area-lock rows. The regression test checks actual parsed region flags and
tracker region assignments against the seed, as well as the recorded counts,
profile selection, schema validation and required feature support.

Contract groups: 0.5.0-0.5.4 use 13db0b3a; 0.5.5-0.5.6 add optional
mineMaterialRoll (8397a952); 0.5.7-0.5.8 also add optional lockHintPlacements
(ffc0f1b5). Missing optional fields keep the current parser's existing defaults.
The absent profile uses the existing legacy selector. Empty dungeonSweeps remains
harmless; dungeonSweepFlags carries the old seed's actual sweep membership.

Check IDs, location regions, item mappings and sweeps remain seed-provided.
Accepting an old contract does not repair old server-side logic or generation
mistakes. Existing save formats are unchanged by this patch. In-game verification
of resume, checks, shops, sweeps and reconnects remains outstanding.
