//! Additional local defeats are ANDed with the finale, never server checked locations.
use serde_json::Value;

/// Absent on old seeds. Malformed requirements must not silently disappear.
pub fn parse(sd: &Value) -> Result<Vec<u32>, &'static str> {
    let Some(raw) = sd.get("options").and_then(|o| o.get("required_boss_flags")) else {
        return Ok(Vec::new());
    };
    let rows = raw
        .as_array()
        .ok_or("required_boss_flags must be an array")?;
    let mut flags = Vec::new();
    for row in rows {
        let flag = row
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or("required_boss_flags needs nonzero u32 flags")?;
        if !flags.contains(&flag) {
            flags.push(flag);
        }
    }
    Ok(flags)
}

/// Zero is a fail-closed sentinel for a malformed requirement, never queried in game.
pub fn all_defeated(flags: &[u32], read: impl Fn(u32) -> bool) -> bool {
    flags.iter().all(|&flag| flag != 0 && read(flag))
}

pub fn label(flag: u32) -> String {
    match flag {
        1252380800 => "Starscourge Radahn".into(),
        20010800 => "Promised Consort Radahn".into(),
        _ => format!("boss defeat flag {flag}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_seed_and_empty_option_add_nothing() {
        for sd in [
            json!({}),
            json!({"options": {}}),
            json!({"options": {"required_boss_flags": []}}),
        ] {
            assert_eq!(parse(&sd), Ok(vec![]));
        }
    }

    #[test]
    fn neither_one_defeat_nor_checked_rewards_complete_both() {
        let flags =
            parse(&json!({"options": {"required_boss_flags": [1252380800, 20010800, 20010800]}}))
                .unwrap();
        assert_eq!(flags, vec![1252380800, 20010800]);
        assert!(!all_defeated(&flags, |_| false));
        for first in &flags {
            assert!(!all_defeated(&flags, |flag| flag == *first));
        }
        assert!(all_defeated(&flags, |_| true));
    }

    #[test]
    fn malformed_requirements_fail_closed() {
        for raw in [
            json!(null),
            json!("20010800"),
            json!([0]),
            json!([-1]),
            json!([4294967296u64]),
            json!([20010800, "oops"]),
        ] {
            assert!(parse(&json!({"options": {"required_boss_flags": raw}})).is_err());
        }
        assert!(!all_defeated(&[0], |_| panic!(
            "zero must never reach the game"
        )));
    }
}
