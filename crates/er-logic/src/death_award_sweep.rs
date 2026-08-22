//! death_award_sweep.rs — the pure half of the missed death-award sweep (clients#385, #395).
//!
//! An EMEVD death-award that misses its one chance — a death `CharacterRatioDead` never witnessed
//! (common 90005300/90005301, `value == 0`), a corpse left unlooted across a reload, or a 1100/1200
//! boss-award latch whose `WaitFor(trigger)` never paid (clients#395: trigger pre-dating the boot
//! suppresses the award) — leaves its check permanently unpayable in-game, while the save carries
//! the signature forever: trigger flag UP, check flag DOWN. The world ships the pair table beside
//! the dll (`death_award_pairs.json`, game data like the check-lot table); the client arm reads
//! the two flags per pair at connect and sets the check flag for every confirmed miss, which the
//! normal check detection then pays. Retroactive by construction — no AP-save archaeology, no
//! slot_data.

use std::collections::HashSet;

/// Parse the shipped table. Refuses malformed shapes loudly: a table that half-parses would
/// silently shrink the sweep, and an absent pair is an unpayable check nobody ever hears about.
pub fn parse_table(text: &str) -> Result<Vec<(u32, u32)>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let pairs = v
        .get("pairs")
        .and_then(|p| p.as_array())
        .ok_or("no `pairs` array")?;
    let mut out = Vec::with_capacity(pairs.len());
    for (i, p) in pairs.iter().enumerate() {
        let death = p.get("death_flag").and_then(|x| x.as_u64());
        let check = p.get("check_flag").and_then(|x| x.as_u64());
        match (death, check) {
            (Some(d), Some(c)) if d <= u32::MAX as u64 && c <= u32::MAX as u64 => {
                out.push((d as u32, c as u32));
            }
            _ => return Err(format!("pair {i} is not a (death_flag, check_flag) object")),
        }
    }
    if out.is_empty() {
        return Err("zero pairs -- an empty table is a generation bug, not a quiet no-op".into());
    }
    Ok(out)
}

/// The pairs THIS seed can sweep: only checks the seed actually polls. A pair whose check flag is
/// not one of the seed's check flags must never be touched — setting a flag the seed does not own
/// is how a sweep on the wrong seed writes somebody's quest state.
pub fn retained(pairs: &[(u32, u32)], seed_check_flags: &HashSet<u32>) -> Vec<(u32, u32)> {
    pairs
        .iter()
        .copied()
        .filter(|(_, check)| seed_check_flags.contains(check))
        .collect()
}

/// One pair's verdict, named so the arm cannot invert it: fire exactly when the death happened
/// and the payment did not.
pub fn missed(death_up: bool, check_up: bool) -> bool {
    death_up && !check_up
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"comment":"x","pairs":[
        {"death_flag":11000497,"check_flag":11007993},
        {"death_flag":10000289,"check_flag":10007085}]}"#;

    #[test]
    fn parses_the_emitted_shape_and_keeps_order() {
        let p = parse_table(GOOD).unwrap();
        assert_eq!(p, vec![(11000497, 11007993), (10000289, 10007085)]);
    }

    #[test]
    fn malformed_refuses_naming_the_pair() {
        assert!(parse_table("{}").is_err());
        assert!(parse_table(r#"{"pairs":[]}"#).is_err());
        let err = parse_table(r#"{"pairs":[{"death_flag":1}]}"#).unwrap_err();
        assert!(err.contains("pair 0"), "{err}");
    }

    #[test]
    fn retained_is_the_seed_intersection() {
        let pairs = parse_table(GOOD).unwrap();
        let seed: HashSet<u32> = [11007993].into_iter().collect();
        assert_eq!(retained(&pairs, &seed), vec![(11000497, 11007993)]);
        assert!(retained(&pairs, &HashSet::new()).is_empty());
    }

    #[test]
    fn missed_fires_exactly_on_death_up_check_down() {
        // THE MOTIVATING CASE: rouqs' Tree Spirit -- dead, unpaid.
        assert!(missed(true, false));
        assert!(!missed(true, true), "already paid: a normal kill");
        assert!(!missed(false, false), "alive: nothing happened yet");
        assert!(
            !missed(false, true),
            "check paid without the death flag: a disjunct paid it"
        );
    }
}
