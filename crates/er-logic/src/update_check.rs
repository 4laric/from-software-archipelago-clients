//! The update verdict (phase 1 of the updater, 2026-08-21): pure halves of the banner that tells
//! a player not just THAT a new release exists, but the thing the changelog's update matrix
//! always knew and the player never saw -- whether it is safe to pick up MID-SEED.
//!
//! The site publishes `/er/latest.json` (`deploy_wizard.sh`, world PR #953): the stable version,
//! its CONTRACT hash from the CONTRACT-VERSIONS ledger, and the release url. The verdict is one
//! comparison: `CONTRACT_HASH` governs compatibility and the version is descriptive, so a newer
//! release with OUR contract is a drop-in even mid-seed, and a newer release whose contract moved
//! means "finish this seed on the pair you have; new seeds need both halves".
//!
//! This module is PURE: parsing, comparison, wording. The fetch (rustls, background thread,
//! fail-silent) lives in the game crate. Support burden provenance: 2026-08-21 alone produced
//! four version-pairing incidents (Tommy, xegpel, Jan's split installs, a stale v0.3.12 pointer).

use er_semver::{compare_semver, parse_semver};
use std::cmp::Ordering;

/// What the fetched `latest.json` said, parsed and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    pub version: String,
    pub contract: String,
    pub url: String,
}

/// Parse `/er/latest.json`. `None` on anything malformed -- a proxy's login page, a truncated
/// body, a field missing. The caller treats `None` as "no news", never as an error a player sees.
pub fn parse_latest(body: &str) -> Option<Latest> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let field = |k: &str| v.get(k)?.as_str().map(str::to_owned);
    let latest = Latest {
        version: field("version")?,
        contract: field("contract")?,
        url: field("url")?,
    };
    // A version that does not parse is a body we do not trust, whatever the other fields say.
    parse_semver(&latest.version).ok()?;
    if latest.contract.len() < 8 || !latest.contract.is_ascii() {
        return None;
    }
    Some(latest)
}

/// The three states a player can be in relative to stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Stable is not newer than this build. Say nothing -- never nag backwards: a dev build or
    /// a lagging site must not tell the player to "update" to something older.
    Current,
    /// A newer stable exists and its contract matches this build: drop-in, safe mid-seed.
    SafeUpdate,
    /// A newer stable exists and the contract MOVED: updating mid-seed breaks the pairing.
    ContractMoved,
}

/// The one comparison. `our_contract` / `latest.contract` are compared on their first 8 hex chars
/// (the prefix every surface of this project prints); case-insensitive.
pub fn verdict(our_version: &str, our_contract: &str, latest: &Latest) -> Verdict {
    let (Ok(ours), Ok(theirs)) = (parse_semver(our_version), parse_semver(&latest.version)) else {
        return Verdict::Current; // an unparseable version is a build this banner cannot judge
    };
    if compare_semver(&theirs, &ours) != Ordering::Greater {
        return Verdict::Current;
    }
    let p8 = |s: &str| s.chars().take(8).collect::<String>().to_ascii_lowercase();
    if p8(our_contract) == p8(&latest.contract) {
        Verdict::SafeUpdate
    } else {
        Verdict::ContractMoved
    }
}

/// The on-screen line. ASCII only (the in-game text rule); `None` when there is nothing to say.
pub fn toast(our_version: &str, our_contract: &str, latest: &Latest) -> Option<String> {
    match verdict(our_version, our_contract, latest) {
        Verdict::Current => None,
        Verdict::SafeUpdate => Some(format!(
            "Archipelago: v{} is out (you have v{}). Same contract -- safe to update even \
             mid-seed. {}",
            latest.version, our_version, latest.url
        )),
        Verdict::ContractMoved => Some(format!(
            "Archipelago: v{} is out with a NEW CONTRACT (you have v{}). Finish this seed on \
             your current client + apworld pair; new seeds need both halves updated. {}",
            latest.version, our_version, latest.url
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = r#"{"version": "0.4.10", "contract": "dc0dc687", "url": "https://github.com/4laric/er-archipelago/releases/tag/v0.4.10"}"#;

    #[test]
    fn the_live_shape_parses() {
        // MOTIVATING CASE: the exact bytes deploy_wizard.sh emitted on 2026-08-21.
        let l = parse_latest(LIVE).expect("the live emission must parse");
        assert_eq!(l.version, "0.4.10");
        assert_eq!(l.contract, "dc0dc687");
    }

    #[test]
    fn a_newer_stable_with_our_contract_is_a_safe_update() {
        let l = parse_latest(LIVE).unwrap();
        assert_eq!(verdict("0.4.9", "dc0dc687", &l), Verdict::SafeUpdate);
        let t = toast("0.4.9", "dc0dc687", &l).unwrap();
        assert!(t.contains("safe to update"), "{t}");
        assert!(
            t.contains("0.4.10") && t.contains("0.4.9"),
            "names both: {t}"
        );
        assert!(t.is_ascii(), "{t}");
    }

    #[test]
    fn a_moved_contract_says_finish_the_seed_first() {
        let l = parse_latest(LIVE).unwrap();
        assert_eq!(verdict("0.4.9", "5c2b9bf2", &l), Verdict::ContractMoved);
        let t = toast("0.4.9", "5c2b9bf2", &l).unwrap();
        assert!(
            t.contains("NEW CONTRACT") && t.contains("Finish this seed"),
            "{t}"
        );
        assert!(t.is_ascii(), "{t}");
    }

    #[test]
    fn never_nag_backwards() {
        // A dev build ahead of stable, and a build exactly AT stable, both stay silent.
        let l = parse_latest(LIVE).unwrap();
        assert_eq!(verdict("0.4.10", "dc0dc687", &l), Verdict::Current);
        assert_eq!(verdict("0.4.11", "dc0dc687", &l), Verdict::Current);
        assert_eq!(
            verdict("0.4.11", "ffffffff", &l),
            Verdict::Current,
            "a contract difference means nothing when stable is not newer"
        );
        assert!(toast("0.4.11", "ffffffff", &l).is_none());
    }

    #[test]
    fn garbage_bodies_are_no_news() {
        // A login page, a truncated body, a missing field, a non-semver version: all None.
        for bad in [
            "<html>login</html>",
            r#"{"version": "0.4.10", "contract": "dc0dc687""#,
            r#"{"version": "0.4.10", "url": "x"}"#,
            r#"{"version": "not-a-version", "contract": "dc0dc687", "url": "x"}"#,
            r#"{"version": "0.5.0", "contract": "short", "url": "x"}"#,
        ] {
            assert!(parse_latest(bad).is_none(), "must reject: {bad}");
        }
    }

    #[test]
    fn contract_compare_is_prefix_and_case_insensitive() {
        let l = parse_latest(r#"{"version": "9.9.9", "contract": "DC0DC687f38de1a6", "url": "x"}"#)
            .unwrap();
        assert_eq!(
            verdict("0.4.10", "dc0dc687aaaaaaaa", &l),
            Verdict::SafeUpdate
        );
    }
}
