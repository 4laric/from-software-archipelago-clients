# Elden Ring event-flag space — validity, persistence, and our reservations

Working reference for choosing event-flag ids the client can **set and read back across a
save/quit/reload**. Written when reserving a band for the save-embedded reconcile marker
(`er-logic/src/marker.rs`); it doubles as the concrete companion to the `er-event-flag-validity`
memory note. Confidence is tagged **[V]** verified in a source / **[I]** inferred.

## Why not every 32-bit id is a flag

`CSEventFlagMan` does **not** allocate storage per arbitrary id. The regulation ships a table of
flag **groups**, each a `(start_id, count)` block; the engine finds the group whose
`[start, start+count)` window contains an id and indexes that group's bit array at `id - start`. An id
with **no containing group has no backing bit** — `set` writes nothing, `get` always reads `false`.
**[V]** (soulsmods `elden-ring-eventparam`; soulsmodding "intro to ER emevd").

This is the `er-event-flag-validity` constraint: an **invented** id silently no-ops. It's also why the
goal sentinel `GOAL_SENTINEL_FLAG = 0x7FFF_0001` (`reconcile_io.rs`) is safe as an inert value — it is
deliberately group-less. **A reserved band must live inside a real, allocated group.**

Global (map-independent) groups relevant to us **[V]**:

| Range | Group purpose | Persisted |
|---|---|---|
| `60000`–`60xxx` | flask / equipment possession | yes |
| `65600`–`65901` | ash-of-war / spell-stone possession | yes |
| `66000`–`66290` | cap-release / upgrade acquisition | yes |
| `67000`–`68500` | crafting-recipe unlocks | yes |
| **`71000`–`75999`** | **bonfire unlock — legacy dungeons** `[71000, 76000)` | **yes** |
| `76000`–`76999` | bonfire unlock — open world | yes |
| `77000` / `78000` | grace guidance / map points | yes |
| `100000`–`100999` | shop-lineup inventory | yes |
| `110000`–`170000+` | shop-NPC inventory (500 each) | yes |

Map-scoped flags are 10-digit ids whose high digits encode a map (`m10`, `m60` overworld tiles) and
low digits a local offset (e.g. boss-defeat `1035500800` = Margit). Within a map block the FromSoft
convention is that low-4-digit offsets **≥ 5000 are volatile** (cleared on event-system reset) while
`0000`–`4999` persist. **[V for DS-family, I for ER]** — this matters for map-scoped picks, not for the
global bonfire group we reserve below.

Reconnect state only needs to survive **quit → reload**, which any non-reset global group gives. (NG+
survival is a stronger property some 6xxxx groups have; we don't need it.) **[I]**

## Ranges this project already occupies

Avoid these when reserving. Inventoried from `er-archipelago` (apworld) and this client; see the source
for the authoritative, generated lists.

| Range | What | Where |
|---|---|---|
| map-scoped 10-digit (`114` … `2_053_487_010`) | vanilla item-acquisition **check** flags | `erw greenfield/msb_flag_region.tsv` |
| `60000`–`69999` | pot / perfume / ash-of-war / crafting **check** flags | `erw greenfield/eldenring/data.py` |
| `62010`–`64xxx`, `82001` | map-reveal flags (authored) | `er-logic/src/reconcile.rs` |
| `71000`–`74351`, `76100`–`76960` | grace warp-unlock / region-open flags | `erw greenfield/grace_flags.tsv`, `region_open_flags.py` |
| `76950` / `76980` | region-lock open / done flags | `er-logic/src/region_lock.rs` |
| `100000`–`100860` | shop-stock event flags | `erw greenfield/eldenring/shoplineup_flags.json` |
| `110000`–`199999` | region check flags + AP tracking flags (`150060…`) | `erw data.py`, `shop_flags.rs` |
| `0x7FFF_0001` | goal sentinel (deliberately group-less, no-op) | `reconcile_io.rs` |

The **gap `75000`–`75999`** is empty on every axis checked: no grace flag (sorted grace flags jump
`74351` → `76100`), no check flag in `71000`–`79999`, no vanilla item flag there, and no reference in
this client. **[V]**

## Reservations

| Band | Owner | Purpose | Status |
|---|---|---|---|
| `75000`–`75119` (120) | `er-logic/src/marker.rs` (`FlagBand::PLACEHOLDER`) | save-embedded reconcile marker (identity + double-buffered watermark) | **PLACEHOLDER — pending Windows verify** |

`75000`–`75119` sits inside the real, save-persisted legacy-bonfire group `[71000, 76000)`, in the
unused tail above vanilla legacy graces (`≤ 74351`), disjoint from everything above. `75120`–`75999`
is headroom for future reservations — **collision-check new ones against this table and the occupied
ranges above** before claiming them.

### Residual risk

1. A future DLC/patch could add legacy graces into `75xxx` (none today). Low.
2. Some non-grace vanilla EMEVD could set a `75xxx` flag; our static `msb`/grace data covers item and
   grace flags, not every event script. **This is the one gap only the in-game test closes.** **[I]**
3. Running alongside thefifthmatt's item randomizer is safe: it allocates dynamically in the `100000+`
   shop group (`minimumGoodFlag=100000`, `fixedShopStart=100400`), not `71000+`. **[V]**

### Windows verify plan (run once before promoting the band out of PLACEHOLDER)

Using the client's own `get_event_flag` / `set_event_flag`:

1. **Vanilla-clean read** — load a normal save (varied progress). Read `75000`–`75119`; expect all
   `false`. Any `true` ⇒ the id is in use — shift the band.
2. **Write** a distinctive pattern across the band.
3. **Persist** — quit to main menu, reload the save, re-read. Expect the exact pattern. This is the
   direct test of `er-event-flag-validity`: an out-of-group band reads back all-`false`.
4. **Non-interference** — clear the band, play normally (rest at graces, fast-travel, pick up items,
   enter/exit a legacy dungeon), re-read. Expect still all-`false`.
5. Repeat step 1 on 2–3 saves at different NG progress.

All pass ⇒ promote `75000`–`75119` to the reserved band (update `FlagBand::PLACEHOLDER` → the audited
base and drop the "PLACEHOLDER" note).

## Is there a community map to reuse?

No published static "unused ER flag space" table exists. The best community evidence — thefifthmatt's
randomizer — allocates **dynamically** (collision-checked forward scan in the `100000+` shop group),
not from a fixed band, and that region is already dense for us. So this doc *is* the artifact: our
occupied-ranges inventory + the reserved band + the group/persistence rules, to be extended as we
reserve more.

## Sources

- soulsmods ER eventparam — <https://soulsmods.github.io/elden-ring-eventparam/>
- soulsmodding EMEVD tutorials — <http://soulsmodding.wikidot.com/tutorial:intro-to-elden-ring-emevd>, <http://soulsmodding.wikidot.com/tutorial:learning-how-to-use-emevd>
- thefifthmatt `PermutationWriter.cs` — <https://github.com/thefifthmatt/SoulsRandomizers/blob/master/RandomizerCommon/PermutationWriter.cs>
- Elden Ring Save Manager, event flags — <https://elden-ring-save-manager.readthedocs.io/en/main/user-guide/event-flags/>
