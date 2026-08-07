//! `boss_grants` -- give the player the tool a specific BOSS assumes they arrived holding,
//! wherever that boss happens to be.
//!
//! MOTIVATING CASE (rule 11), issue #413. Rykard's second phase is built around the
//! **Serpent-Hunter**, a unique great spear that vanilla parks in Volcano Manor on the way to him.
//! A randomiser scatters that spear into the multiworld, so the fight can demand a tool the player
//! has no way to hold. boblerrr, 2026-08-06: *"rykard without serpent hunter is some bs"*.
//!
//! # 🛑 KEYED ON THE CHARACTER, NOT THE PLACE -- and that is the whole design
//!
//! A first cut keyed this on Rykard's ARENA (play_region bucket 16000, m16). **Alaric rejected it,
//! 2026-08-06: it has to fire whenever you fight Rykard, no matter where Rykard is.** An enemy
//! randomiser moves bosses between arenas, so a place key grants the spear to whoever inherited
//! his room and gives Rykard's actual opponent nothing. The place is the one thing about this
//! fight that is NOT stable.
//!
//! What IS stable is the character. `NpcParam` ids are CHR-ENCODED (`CCCC____`), so every row
//! belonging to chr `c4710` identifies Rykard no matter which arena he was spawned into, and a
//! prefix test on [`RYKARD_CHR_ID`] is the whole gate. Both phases are that one character --
//! `c4700` has no rows at all -- so this covers the serpent as well, which matters because the
//! spear is the answer to BOTH phases.
//!
//! 🛑 I first keyed this on `NpcParam.nameId` and got the **Twin Maiden Husks**; see
//! [`RYKARD_NPC_PARAM_ROWS`] for how a single id resolving in the table I expected passed for
//! evidence, and what to check instead.
//!
//! # Three properties, each load-bearing
//!
//! 1. **The grant never collects the check.** The Serpent-Hunter's vanilla obtained-flag is
//!    [`SERPENT_HUNTER_CHECK_FLAG`] (16007690) -- exactly what apworld check **7771816** is keyed
//!    on. Latching on it the way `unique_grants` latches the Steed Whistle on 60100 would SEND the
//!    check the moment we granted the copy, paying the multiworld for an item the player never
//!    found. That is the filed "start grant flags are check flags" defect. A test greps this
//!    module's own body for the flag verbs so the property cannot rot.
//!
//! 2. **The latch is POSSESSION.** A bag read for any reinforce level of the base row. Nothing
//!    allocated, nothing persisted; correct across reload, save-scum, a fresh character, or the
//!    pool copy arriving first.
//!
//! 3. **"Don't know" is never "no".** BOTH inputs are `Option<bool>`, and either one being `None`
//!    (character set unwalkable / bag unresolvable this tick) must behave like "do nothing". The
//!    other direction duplicates a unique weapon every tick until the read comes back. A missed
//!    grant simply retries on the next one.

use crate::hook::GameHook;

/// Serpent-Hunter, base (+0) weapon row. Reinforce levels occupy `base ..= base + 25` inside the
/// 100-wide stride, which is why possession is tested on the BASE.
pub const SERPENT_HUNTER_BASE: i32 = 17030000;

/// ER's weapon id stride per smithing level. Mirrors `eldenring-archipelago`'s `REINFORCE_STEP`;
/// duplicated because that crate does not build off Windows.
pub const REINFORCE_STEP: i32 = 100;

/// Rykard's CHR id. Both phases are this one character: `NpcName` **904710000**
/// ("God-Devouring Serpent") and **904710001** ("Rykard, Lord of Blasphemy") are name index 0 and
/// 1 of chr `c4710`, and `c4700` has **no `NpcParam` rows at all**. So the serpent is not a second
/// character and this key covers the whole fight -- which matters, because the spear is the answer
/// to BOTH phases, not just the second (Alaric, 2026-08-06).
pub const RYKARD_CHR_ID: i32 = 4710;

/// 🛑🛑 HOW I GOT THIS WRONG THE FIRST TIME, because the same trap is one keystroke away.
///
/// I keyed this on `NpcParam.nameId`, having found that row `500020079` carries `nameId 160000`
/// and that **PlaceName** 160000 is "Rykard, Lord of Blasphemy". Both facts are true and the
/// conclusion was still wrong: `nameId` indexes **NpcName**, not PlaceName, and NpcName 160000 is
/// the **Twin Maiden Husks**. Two id spaces, one collision, and the shipped build granted the
/// spear in Roundtable Hold.
///
/// The check that would have caught it in one line: resolve the SAME id in both tables and see
/// that they disagree, or resolve a handful of OTHER `nameId`s (134800 Millicent, 121600 Blaidd,
/// 130900 Patches -- all NpcName, none in PlaceName). A single id that resolves in the table you
/// expected is not evidence that the table is the right one.
///
/// Boss rows carry `nameId 0`; a boss healthbar name comes from the EMEVD `DisplayBossHealthBar`
/// call, not from `NpcParam`. So there was never going to be a name-keyed answer here.
///
/// What replaced it is structural: `NpcParam` ids are CHR-ENCODED, `CCCC____`. Verified on two
/// independent characters -- `c4710` has 6 rows (47100000, 47100038, 47101000, 47101038, 47102000,
/// 47109000) and Torrent `c8000` has exactly one (80000000).
pub const RYKARD_NPC_PARAM_ROWS: &[i32] =
    &[47100000, 47100038, 47101000, 47101038, 47102000, 47109000];

/// 🛑 THE FLAG THIS MODULE MUST NEVER TOUCH -- see property 1.
pub const SERPENT_HUNTER_CHECK_FLAG: u32 = 16007690;

/// Does `row` (a resolved weapon param row, base + level) belong to `base`?
pub fn is_level_of(row: i32, base: i32) -> bool {
    row - (row % REINFORCE_STEP) == base
}

/// Does this `NpcParam` row belong to chr `chr_id`? Ids are chr-encoded `CCCC____`.
///
/// ⭐ A PREFIX test, not a membership test in [`RYKARD_NPC_PARAM_ROWS`]: a patch that adds a
/// seventh `c4710` row should be covered automatically, and the enumerated list is documentation
/// of what exists today rather than the gate.
///
/// 🛑 `npc_param_id`, NEVER `npc_id`. `npc_id` is the 4-digit CHR id (4710 here) and the two id
/// spaces OVERLAP -- passing one where the other belongs is how phase 1a shipped a silently wrong
/// native tier.
pub fn is_character(npc_param_id: i32, chr_id: i32) -> bool {
    npc_param_id / 10_000 == chr_id
}

/// Is this row one of Rykard's, in either phase?
pub fn is_rykard(npc_param_id: i32) -> bool {
    is_character(npc_param_id, RYKARD_CHR_ID)
}

/// THE DECISION. `Some(full_id)` = grant now; `None` = do nothing.
///
/// `present` = is the character loaded, `holds` = does the player already have one. Either input
/// being `None` means the read failed this tick, and both failure modes resolve to "do nothing"
/// (property 3).
pub fn boss_grant_action(present: Option<bool>, holds: Option<bool>) -> Option<i32> {
    match (present, holds) {
        (Some(true), Some(false)) => Some(SERPENT_HUNTER_BASE),
        _ => None,
    }
}

/// Is the boss healthbar currently showing `chr_id`?
///
/// ⭐⭐⭐ THE FIGHT, NOT THE NEIGHBOURHOOD. [`crate::boss_grants`]'s other input,
/// `any_character_present`, is a LOAD test -- it answers "is c4710 instantiated anywhere", which is
/// true from the moment the area streams in. bobler got the Serpent-Hunter walking into a grace
/// with the boss nowhere in sight (2026-08-07), and the weapon HOLD armed just as early, for a
/// fight that had not started. `GameDataMan.boss_health_bar_npc_param_id` is the game's own
/// statement that the fight is happening right now, and it is already in the id space
/// [`is_character`] speaks, so no new key and no distance maths.
///
/// `healthbar_npc_param_id`: `None` = `GameDataMan` was unreadable this tick (main menu, mid-load).
/// 🛑 `Some(0)` is "no boss bar is up" and MUST NOT reach [`is_character`], which would divide it
/// down to chr 0 and match any `npc_param_id` below 10000.
///
/// Follows property 3 of this module: "don't know" is never "no". `None` propagates.
pub fn healthbar_shows(chr_id: i32, healthbar_npc_param_id: Option<i32>) -> Option<bool> {
    let id = healthbar_npc_param_id?;
    Some(id != 0 && is_character(id, chr_id))
}

/// The `CCCC____` decode, but only where that shape is actually defensible.
///
/// ⭐⭐⭐ [`is_character`] divides by `10_000` unconditionally. That is correct for the rows this
/// module GATES on -- every `c4710` row is 8 digits, and `id / 10_000 == 4710` can only hold for an
/// 8-digit id -- and it is WRONG as a general decode. bobler's 2026-08-07 log carries 9-digit rows
/// (`523610066`, `523250066`, and `523680065` in the scaling logs); dividing those by `10_000`
/// yields "chr 52361", a five-digit character that does not exist. The GATE is unharmed by this,
/// but a DIAGNOSTIC that prints the quotient invents a character and sends its reader hunting one.
///
/// 🛑 DO NOT "FIX" THIS BY GUESSING THE 9-DIGIT SPLIT. Nothing here has measured whether those rows
/// are `CCCC_____` or `CCCCC____`, and a diagnostic that states an unmeasured decode as fact is
/// worse than one that declines. Until something measures it, the honest output is no chr at all.
pub fn decode_chr(npc_param_id: i32) -> Option<i32> {
    (10_000_000..=99_999_999)
        .contains(&npc_param_id)
        .then_some(npc_param_id / 10_000)
}

/// Should the spear go into the player's hand for THIS fight -- and what does the latch become?
///
/// Returns `(enqueue_now, latched_after)`.
///
/// ⭐⭐⭐ MOTIVATING CASE (rule 11), boblerrr 2026-08-07 16:10:50, and it is the diagnostic's own
/// output that caught it:
///
/// ```text
/// boss-grant: healthbar npc_param 47101038 = chr 4710, IS Rykard | c4710 loaded = yes |
///             already holds the spear = yes -> no grant
/// ```
///
/// The real Rykard, the real spear in the bag, and nothing in his hand. The equip rode on the
/// one-shot GRANT, so it fired exactly once per character, ever: reload, re-fight, or simply
/// swapping weapons after the grant all left the player facing the fight without the tool the
/// grant exists to provide. The grant answers "does the player own one"; this answers "is it in
/// their hand for the fight", and they are different questions.
///
/// 🛑 ONCE PER FIGHT, via `latched`. Without it this re-enqueues every tick the bar is up, which
/// would also stomp a deliberate mid-fight swap over and over rather than once.
///
/// 🛑 Property 3 of this module, twice over. `fight_on == None` (GameDataMan down) acts on nothing
/// and CHANGES nothing -- it must not re-arm the latch, or a blink at the wrong moment re-equips
/// mid-fight. `holds == None` (bag unresolvable) leaves the latch OPEN so the next tick retries,
/// because latching on a failed read would silently skip the fight entirely.
pub fn equip_for_fight(fight_on: Option<bool>, holds: Option<bool>, latched: bool) -> (bool, bool) {
    match fight_on {
        // The fight is over (or never started): re-arm for the next one.
        Some(false) => (false, false),
        // We could not look this tick. Do nothing, and leave the latch exactly as it was.
        None => (false, latched),
        Some(true) => match (latched, holds) {
            (true, _) => (false, true),
            (false, Some(true)) => (true, true),
            // No spear in the bag, or the bag would not answer -- retry on the next tick. The
            // grant path may be handing one over right now.
            (false, _) => (false, false),
        },
    }
}

/// Render an `Option<bool>` read for a human. `None` is a FAILED READ, never a "no".
fn say(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unreadable this tick",
    }
}

/// Pack the two inputs the diagnostic keys on into one comparable value.
///
/// The caller lives in the Windows-only crate, which no test here can reach, so it gets exactly one
/// atomic and zero branching: swap this key, compare, log on a change. Equality is the only
/// operation -- the packing is not an id and nothing should decode it.
pub fn diag_key(healthbar_npc_param_id: Option<i32>, present: Option<bool>) -> i64 {
    let bar = healthbar_npc_param_id.unwrap_or(i32::MIN) as i64;
    let pres = match present {
        None => 2,
        Some(false) => 0,
        Some(true) => 1,
    };
    (bar << 2) | pres
}

/// One line saying why the spear did -- or did not -- change hands.
///
/// ⭐⭐⭐ MOTIVATING CASE (rule 11), boblerrr 2026-08-07: *"no spear whatsoever"*. A NON-grant is
/// completely silent. [`boss_grant_action`] returns `None` for three unrelated situations and the
/// log cannot tell them apart:
///
/// 1. **Rykard was never loaded.** Under an enemy randomiser the boss standing in Rykard's arena is
///    usually not `c4710` at all, so `present` is honestly `false` and the design is working. The
///    player, looking at a boss healthbar that says Rykard, reports a bug.
/// 2. **The player already holds one.** The equip rides on the grant, so a RE-fight gets nothing.
/// 3. **A read failed** this tick and the next tick will retry.
///
/// Triage of the 2026-08-07 log stalled on exactly this: 13k lines, and the only admissible
/// evidence about Rykard was two lines from the grants that DID fire. This makes the negative
/// case speak.
///
/// 🛑 THE VERDICT IS TAKEN FROM [`boss_grant_action`], NEVER RE-DERIVED. A diagnostic that
/// reimplements the decision it reports on can disagree with it, and then the log is worse than
/// nothing. `the_verdict_can_never_disagree_with_the_decision` pins that across the whole matrix.
///
/// Returns `None` when there is nothing to say -- no boss bar up and no `c4710` loaded -- so the
/// caller can log unconditionally.
pub fn grant_diagnosis(
    healthbar_npc_param_id: Option<i32>,
    present: Option<bool>,
    holds: Option<bool>,
) -> Option<String> {
    // 🛑 `Some(0)` is "no bar is up", and it must never reach `is_character` -- see
    // `healthbar_shows`. Filtering it here keeps chr 0 out of the rendered line as well.
    let bar = healthbar_npc_param_id.filter(|&id| id != 0);
    if bar.is_none() && present != Some(true) {
        return None;
    }
    let granting = boss_grant_action(present, holds).is_some();
    let bar_txt = match bar {
        Some(id) if is_rykard(id) => {
            format!("healthbar npc_param {id} = chr {}, IS Rykard", id / 10_000)
        }
        Some(id) => match decode_chr(id) {
            Some(chr) => format!("healthbar npc_param {id} = chr {chr}, NOT Rykard (c{RYKARD_CHR_ID})"),
            // 🛑 Not the CCCC____ shape, so there is no chr to name. Saying so beats printing a
            // quotient that looks like a character id and is not one -- see `decode_chr`.
            None => format!(
                "healthbar npc_param {id} = chr NOT DECODED (not the 8-digit CCCC____ shape), NOT Rykard (c{RYKARD_CHR_ID})"
            ),
        },
        None => "no boss healthbar up".to_string(),
    };
    let mut line = format!(
        "boss-grant: {bar_txt} | c{RYKARD_CHR_ID} loaded = {} | already holds the spear = {} -> {}",
        say(present),
        say(holds),
        if granting { "GRANTING" } else { "no grant" }
    );
    if !granting {
        if holds == Some(true) {
            line.push_str(
                " <- you already have a copy, so nothing is granted AND nothing is auto-equipped: \
                 the equip rides on the grant, so a second fight gets no spear in hand.",
            );
        } else if bar.is_some_and(|id| !is_rykard(id)) {
            line.push_str(
                " <- if you are fighting Rykard right now then an enemy randomiser has replaced the \
                 character. This grant is keyed on c4710 and never on the arena, so the spear \
                 followed Rykard to wherever he actually is.",
            );
        }
    }
    Some(line)
}

/// Should WEAPON auto-equips be held while this fight is on?
///
/// ⭐⭐⭐ THE SEQUENCING IS THE WHOLE FUNCTION. Pausing on the same tick the spear is enqueued
/// would hold the spear too, and the player would end up fighting Rykard with the grant sitting
/// unequipped in the bag -- the exact outcome the grant exists to prevent. So the pause waits for
/// an EMPTY queue: the spear drains on one tick, the gate closes on the next. `paused` is fed back
/// in so that once closed it STAYS closed as the queue refills with incoming weapons.
///
/// 🛑 `None` (we could not read the character set, or could not act at all this tick) resolves to
/// NOT paused. Resuming early is the status quo behaviour and costs a clobber; staying paused on a
/// stale read would silently block every weapon equip for the rest of the session.
///
/// WEAPONS ONLY, and NARROW ON PURPOSE. `auto_equip`'s own module doc says clobbering the weapon
/// in your hand mid-boss "IS the feature, not a bug to guard against" -- the French Challenge
/// premise. The carve-out is therefore scoped to exactly the fight where WE handed the player a
/// tool, and to the one category that can lose it for them: armour and talismans arriving mid-fight
/// are harmless and keep flowing. Nothing is dropped either way -- a held weapon equips the moment
/// the fight ends.
pub fn should_pause_weapon_equips(present: Option<bool>, queue_empty: bool, paused: bool) -> bool {
    match present {
        Some(true) => queue_empty || paused,
        _ => false,
    }
}

/// Production adapter. A `false` from `grant_full_id` means no inventory pointer this tick; we say
/// nothing and the next tick retries, which is why the POSSESSION latch and not a "we tried" flag
/// is what stops the second copy.
pub fn tick(hook: &mut dyn GameHook, present: Option<bool>, holds: Option<bool>) -> Option<String> {
    let full_id = boss_grant_action(present, holds)?;
    if hook.grant_full_id(full_id, 1) {
        Some("Serpent-Hunter granted for Rykard".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_equip_follows_the_fight_not_the_grant() {
        // MOTIVATING CASE (rule 11), boblerrr 2026-08-07 16:10:50, caught by this module's own
        // diagnostic: the real Rykard on screen, the real spear in the bag, nothing in hand,
        // because the equip rode on a grant that had fired hours and one reload earlier.
        assert_eq!(equip_for_fight(Some(true), Some(true), false), (true, true));
    }

    #[test]
    fn one_equip_per_fight_not_one_per_tick() {
        // 🛑 Without the latch this re-enqueues every tick the bar is up, which also means stomping
        // a deliberate mid-fight swap over and over instead of once.
        let (enqueue, latched) = equip_for_fight(Some(true), Some(true), false);
        assert!(enqueue);
        for _ in 0..100 {
            assert_eq!(
                equip_for_fight(Some(true), Some(true), latched),
                (false, true)
            );
        }
    }

    #[test]
    fn the_latch_rearms_when_the_fight_ends() {
        // Rykard dies, or the player flees, or dies themselves: all read as "bar is down", and all
        // of them must leave the next fight able to re-equip.
        assert_eq!(
            equip_for_fight(Some(false), Some(true), true),
            (false, false)
        );
        assert_eq!(equip_for_fight(Some(false), None, true), (false, false));
        // ...and the fight after that equips again.
        assert_eq!(equip_for_fight(Some(true), Some(true), false), (true, true));
    }

    #[test]
    fn dont_know_acts_on_nothing_and_rearms_nothing() {
        // Property 3. A `None` from GameDataMan is a blink, not a fight ending -- re-arming on it
        // would re-equip mid-fight the moment the read came back.
        assert_eq!(equip_for_fight(None, Some(true), true), (false, true));
        assert_eq!(equip_for_fight(None, Some(true), false), (false, false));
        assert_eq!(equip_for_fight(None, None, true), (false, true));
    }

    #[test]
    fn a_bag_that_will_not_answer_retries_instead_of_latching() {
        // 🛑 The asymmetry with the clause above is deliberate. Latching on an unreadable BAG would
        // skip the whole fight; leaving it open costs one retry per tick until the read lands.
        assert_eq!(equip_for_fight(Some(true), None, false), (false, false));
        // No spear at all is not a defect and not an equip -- the grant path may be mid-handover.
        assert_eq!(
            equip_for_fight(Some(true), Some(false), false),
            (false, false)
        );
    }

    #[test]
    fn the_nine_digit_rows_from_boblers_log_are_not_decoded() {
        // 🛑 THE REGRESSION THIS EXISTS FOR. These three are verbatim from the 2026-08-07 log, and
        // `id / 10_000` renders them as "chr 52361" / "chr 52325" / "chr 52368" -- five-digit
        // characters that do not exist. The gate is untouched (none can equal 4710); the DISPLAY
        // was inventing a character.
        for id in [523610066, 523250066, 523680065] {
            assert_eq!(decode_chr(id), None, "id {id}");
            assert!(!is_rykard(id), "id {id}");
        }
        let d = grant_diagnosis(Some(523610066), Some(false), Some(true)).unwrap();
        assert!(d.contains("NOT DECODED"), "{d}");
        assert!(!d.contains("chr 52361"), "{d}");
    }

    #[test]
    fn the_eight_digit_rows_still_decode() {
        // Everything the log actually decoded correctly must keep doing so.
        for (id, chr) in [
            (47101038, 4710),
            (53600000, 5360),
            (48000068, 4800),
            (46300912, 4630),
        ] {
            assert_eq!(decode_chr(id), Some(chr), "id {id}");
        }
        // 🛑 Every Rykard row is 8-digit BY CONSTRUCTION -- `id / 10_000 == 4710` cannot hold
        // otherwise -- so the gate and the decode can never disagree about this character.
        for &row in RYKARD_NPC_PARAM_ROWS {
            assert_eq!(decode_chr(row), Some(RYKARD_CHR_ID), "row {row}");
        }
    }

    #[test]
    fn the_swapped_in_boss_names_itself() {
        // MOTIVATING CASE (rule 11), boblerrr 2026-08-07: "no spear whatsoever". He is at a boss he
        // believes is Rykard; the character actually loaded is somebody else's. Before this the log
        // said NOTHING at all, and triage could not separate it from a real miss.
        let d = grant_diagnosis(Some(53600000), Some(false), Some(false)).unwrap();
        assert!(d.contains("chr 5360"), "{d}");
        assert!(d.contains("NOT Rykard (c4710)"), "{d}");
        assert!(d.contains("no grant"), "{d}");
        assert!(d.contains("enemy randomiser"), "{d}");
        // 🛑 It must not accuse the player's own build of anything, and must not claim the bag.
        assert!(!d.contains("already have a copy"), "{d}");
    }

    #[test]
    fn a_real_rykard_bar_that_grants_says_so() {
        for &row in RYKARD_NPC_PARAM_ROWS {
            let d = grant_diagnosis(Some(row), Some(true), Some(false)).unwrap();
            assert!(d.contains("IS Rykard"), "row {row}: {d}");
            assert!(d.contains("GRANTING"), "row {row}: {d}");
            assert!(!d.contains("enemy randomiser"), "row {row}: {d}");
        }
    }

    #[test]
    fn the_refight_case_is_the_one_that_looks_like_a_bug() {
        // Rykard is right there, the player has the spear in the bag, and NOTHING happens -- no
        // grant and, because the equip rides on the grant, no equip either. Indistinguishable from
        // a broken grant unless the line says so.
        let d = grant_diagnosis(Some(47101000), Some(true), Some(true)).unwrap();
        assert!(d.contains("IS Rykard"), "{d}");
        assert!(d.contains("no grant"), "{d}");
        assert!(d.contains("already have a copy"), "{d}");
    }

    #[test]
    fn silent_when_there_is_nothing_to_say() {
        // No bar, nobody loaded: the overwhelming majority of ticks. The caller logs
        // unconditionally, so the quiet has to live in here.
        assert_eq!(grant_diagnosis(Some(0), Some(false), Some(false)), None);
        assert_eq!(grant_diagnosis(None, None, None), None);
        assert_eq!(grant_diagnosis(Some(0), None, Some(true)), None);
        // ...but a field-spawned Rykard has NO healthbar at all, and that case must still report.
        assert!(grant_diagnosis(Some(0), Some(true), Some(false)).is_some());
        assert!(grant_diagnosis(None, Some(true), Some(false)).is_some());
    }

    #[test]
    fn an_empty_bar_is_never_rendered_as_chr_zero() {
        // 🛑 THE ZERO TRAP AGAIN, on the rendering side. `Some(0) / 10_000` is chr 0, and printing
        // "chr 0, NOT Rykard" would send the next reader hunting a character that does not exist.
        let d = grant_diagnosis(Some(0), Some(true), Some(false)).unwrap();
        assert!(d.contains("no boss healthbar up"), "{d}");
        assert!(!d.contains("chr 0"), "{d}");
    }

    #[test]
    fn the_verdict_can_never_disagree_with_the_decision() {
        // 🛑 A diagnostic that re-derives the decision it reports can drift out of step with it,
        // and a log that lies is worse than a log that is silent. Sweep the whole input matrix and
        // assert the rendered verdict is a pure function of `boss_grant_action`.
        let bars = [None, Some(0), Some(47101000), Some(53600000)];
        let tri = [None, Some(false), Some(true)];
        for bar in bars {
            for present in tri {
                for holds in tri {
                    let Some(d) = grant_diagnosis(bar, present, holds) else {
                        continue;
                    };
                    let decided = boss_grant_action(present, holds).is_some();
                    assert_eq!(
                        d.contains("GRANTING"),
                        decided,
                        "bar {bar:?} present {present:?} holds {holds:?}: {d}"
                    );
                }
            }
        }
    }

    #[test]
    fn diag_key_moves_when_either_input_moves() {
        // The caller keeps ONE atomic and logs on a change, so a key that collapsed two distinct
        // states would silently swallow the transition between them.
        let base = diag_key(Some(47101000), Some(true));
        assert_ne!(base, diag_key(Some(47101000), Some(false)));
        assert_ne!(base, diag_key(Some(47101000), None));
        assert_ne!(base, diag_key(Some(53600000), Some(true)));
        assert_ne!(base, diag_key(None, Some(true)));
        assert_ne!(diag_key(Some(0), None), diag_key(None, None));
        // Stable for an unchanged pair -- otherwise it would log every tick.
        assert_eq!(base, diag_key(Some(47101000), Some(true)));
        // Nothing may collide with the caller's "nothing reported yet" sentinel.
        for bar in [None, Some(0), Some(47101000), Some(i32::MIN)] {
            for present in [None, Some(false), Some(true)] {
                assert_ne!(diag_key(bar, present), i64::MIN, "{bar:?} {present:?}");
            }
        }
    }

    #[test]
    fn the_healthbar_names_the_fight_not_the_neighbourhood() {
        // MOTIVATING CASE (rule 11): the hold used to key on `any_character_present`, which is a
        // LOAD test -- true from the moment the area streams in. Under an enemy randomiser that
        // also meant walking into a grace announced where Rykard had been swapped to.
        for &row in RYKARD_NPC_PARAM_ROWS {
            assert_eq!(
                healthbar_shows(RYKARD_CHR_ID, Some(row)),
                Some(true),
                "row {row}"
            );
        }
        // Torrent's only row -- a real npc_param_id that is not this fight.
        assert_eq!(healthbar_shows(RYKARD_CHR_ID, Some(80000000)), Some(false));
    }

    #[test]
    fn no_healthbar_is_a_no_not_a_match_on_chr_zero() {
        // 🛑 THE ZERO TRAP. `is_character` is `npc_param_id / 10_000 == chr_id`, so a raw 0 would
        // divide down to chr 0 -- and any caller asking about a low chr id would read "the fight is
        // on" from an EMPTY healthbar. The guard is the `id != 0`, and this is what pins it.
        assert_eq!(healthbar_shows(RYKARD_CHR_ID, Some(0)), Some(false));
        assert_eq!(
            healthbar_shows(0, Some(0)),
            Some(false),
            "chr 0 must not match an empty bar"
        );
    }

    #[test]
    fn an_unreadable_game_data_man_is_dont_know_not_no() {
        // Property 3. `None` must propagate rather than collapsing to `false`, so the caller can
        // distinguish "no fight" from "could not look" -- they lead to the same pause decision
        // today, but a silent collapse is how that stops being true by accident.
        assert_eq!(healthbar_shows(RYKARD_CHR_ID, None), None);
    }

    #[test]
    fn the_hold_now_follows_the_healthbar_end_to_end() {
        // The composition the caller actually performs, asserted as one unit -- the two halves
        // were each correct before and the DEFECT was in how they were wired together.
        let hb = |id| healthbar_shows(RYKARD_CHR_ID, id);
        // area loaded, no bar up: NOT held -- this is the case that used to hold
        assert!(!should_pause_weapon_equips(hb(Some(0)), true, false));
        // bar up, queue drained: held
        assert!(should_pause_weapon_equips(hb(Some(47100000)), true, false));
        // bar up but the queue still has the spear in it: NOT held yet
        assert!(!should_pause_weapon_equips(
            hb(Some(47100000)),
            false,
            false
        ));
        // GameDataMan down: released, never stranded
        assert!(!should_pause_weapon_equips(hb(None), true, true));
    }

    #[test]
    fn every_known_rykard_row_matches_and_neighbours_do_not() {
        for &row in RYKARD_NPC_PARAM_ROWS {
            assert!(is_rykard(row), "{row} is a c4710 row");
        }
        // Torrent (c8000, row 80000000) is the second character the CCCC____ convention was
        // verified on; it must never match.
        assert!(!is_rykard(80000000));
        assert!(is_character(80000000, 8000));
        assert!(!is_rykard(47110000), "c4711 is a different character");
        assert!(!is_rykard(0));
    }

    /// THE REGRESSION. 500020079 is the Twin Maiden Husks -- the row a PlaceName/NpcName id
    /// collision put here first, which granted the spear in Roundtable Hold.
    #[test]
    fn the_twin_maiden_husks_row_is_not_rykard() {
        assert!(!is_rykard(500020079));
    }

    #[test]
    fn reinforce_levels_all_belong_to_the_base() {
        for level in 0..=25 {
            assert!(is_level_of(
                SERPENT_HUNTER_BASE + level,
                SERPENT_HUNTER_BASE
            ));
        }
        assert!(!is_level_of(SERPENT_HUNTER_BASE + 100, SERPENT_HUNTER_BASE));
        assert!(!is_level_of(SERPENT_HUNTER_BASE - 1, SERPENT_HUNTER_BASE));
    }

    #[test]
    fn decision_truth_table() {
        let want = Some(SERPENT_HUNTER_BASE);
        assert_eq!(boss_grant_action(Some(true), Some(false)), want);
        assert_eq!(boss_grant_action(Some(true), Some(true)), None);
        assert_eq!(boss_grant_action(Some(false), Some(false)), None);
        assert_eq!(boss_grant_action(Some(false), Some(true)), None);
    }

    /// PROPERTY 3, its own test on BOTH inputs: the wrong arm duplicates a unique weapon forever.
    #[test]
    fn an_unknown_read_never_grants() {
        for holds in [Some(true), Some(false), None] {
            assert_eq!(
                boss_grant_action(None, holds),
                None,
                "unknown presence granted"
            );
        }
        for present in [Some(true), Some(false), None] {
            assert_eq!(
                boss_grant_action(present, None),
                None,
                "unknown bag granted"
            );
        }
    }

    /// PROPERTY 1, enforced mechanically: no flag verb may appear in this module's body, because
    /// the only flag in scope is the one check 7771816 is keyed on.
    #[test]
    fn the_check_flag_is_never_read_or_written() {
        assert_eq!(SERPENT_HUNTER_CHECK_FLAG, 16007690);
        let src = include_str!("boss_grants.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for verb in ["get_event_flag", "set_event_flag"] {
            assert!(
                !body.contains(verb),
                "boss_grants calls {verb} -- the only flag in scope is check 7771816's, and \
                 setting it collects a check the player never found"
            );
        }
    }

    /// Repeat ticks while the boss is loaded settle: one grant, then the bag read stops it.
    #[test]
    fn repeat_ticks_grant_exactly_once() {
        let mut held = false;
        let mut grants = 0;
        for _ in 0..10 {
            if boss_grant_action(Some(true), Some(held)).is_some() {
                grants += 1;
                held = true;
            }
        }
        assert_eq!(grants, 1);
    }

    /// THE SEQUENCING, as a timeline: the spear must get out of the queue before the gate shuts.
    #[test]
    fn the_granted_spear_drains_before_the_pause_closes() {
        let mut paused = false;
        // Tick 1: Rykard loads, the spear was just enqueued -> queue NOT empty -> do not pause.
        paused = should_pause_weapon_equips(Some(true), false, paused);
        assert!(!paused, "pausing here would hold the spear we just granted");
        // Tick 2: the spear equipped, queue drained -> now close the gate.
        paused = should_pause_weapon_equips(Some(true), true, paused);
        assert!(paused);
        // Tick 3+: a weapon arrives from another world; the queue is no longer empty, and the gate
        // must STAY shut -- this is the case `paused` is fed back in for.
        paused = should_pause_weapon_equips(Some(true), false, paused);
        assert!(paused, "an incoming weapon reopened the gate mid-fight");
    }

    #[test]
    fn the_pause_lifts_when_the_fight_ends_or_the_read_fails() {
        assert!(
            !should_pause_weapon_equips(Some(false), true, true),
            "boss gone -> resume"
        );
        assert!(
            !should_pause_weapon_equips(None, true, true),
            "unknown -> resume, never latch"
        );
    }

    #[test]
    fn the_pause_never_starts_without_the_boss() {
        for q in [true, false] {
            assert!(!should_pause_weapon_equips(Some(false), q, false));
            assert!(!should_pause_weapon_equips(None, q, false));
        }
    }
}
