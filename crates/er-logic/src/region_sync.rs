//! Region Sync (er-archipelago#1005) -- the pure half.
//!
//! A DeathLink-shaped opt-in link for seamless co-op. Under ERSC every player is in ONE physical
//! world (the co-op host's) but on their own AP slot, so if player A has unlocked Liurnia and
//! player B has not, B is region-kicked out from under A. Region Sync broadcasts a slot's
//! region-OPEN and every opted-in slot applies the same region-open flag locally.
//!
//! 🛑 THIS IS AN ACCESS CONVENIENCE, NOT A LOGIC CHANGE. Applying a synced open is exactly what
//! the console's `!setflag <region open flag> 1` does today: it opens the door. It does NOT grant
//! the AP region-Lock item (that stays wherever Fill put it), it does not touch the receive
//! watermark, and it does not move anyone's logic or goal state. Each slot still has to find its
//! own Lock for its own item/goal accounting -- the sync only stops the follower being kicked.
//!
//! This module owns the wire shape and the two decisions that must never be gotten wrong (do not
//! echo your own broadcast; do not rebroadcast an open you were handed), so both are host-tested.
//! The game-side glue -- setting the flag and lighting the graces -- lives in the client crate's
//! `region::open_on_received_name`, the same call a locally received Lock makes.

use std::collections::HashSet;

use serde_json::{Value, json};

/// Bounce tag. Its own link group, like `DeathLink` / `TrapLink`: a slot that did not opt in never
/// sees these packets, and no other game speaks this tag.
pub const TAG: &str = "RegionSync";

/// One inbound region-open, already vetted: not ours, and both fields present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    /// The broadcasting slot, for the console line. Untrusted display text.
    pub source: String,
    /// Region name as the seed's `regionOpenFlags` knows it, minus the ` Lock` suffix -- i.e. the
    /// same string the local "Region unlocked: X" line carries.
    pub region: String,
}

/// The packet a slot broadcasts when one of its regions actually opened.
pub fn encode_open(source: &str, region: &str, time: f64) -> Value {
    json!({ "time": time, "source": source, "region": region })
}

/// Decode an inbound `RegionSync` bounce.
///
/// `None` (silently) when the packet is ours: a Bounce with a tag goes to EVERY member of the
/// group including the sender, so without this the applier would re-handle its own open. `None`
/// (for the caller to log) when a field is missing -- tolerant like every other wire parse here,
/// a malformed packet is skipped rather than fatal.
pub fn parse_open(data: &Value, my_name: Option<&str>) -> Option<Inbound> {
    let source = data.get("source").and_then(Value::as_str)?;
    if my_name == Some(source) {
        return None;
    }
    let region = data.get("region").and_then(Value::as_str)?;
    if region.is_empty() {
        return None;
    }
    Some(Inbound {
        source: source.to_string(),
        region: region.to_string(),
    })
}

/// Which of this tick's region-opens to broadcast.
///
/// 🛑 THE ANTI-ECHO IS STRUCTURAL, NOT A TIMER. `applied_from_link` is the set of regions this
/// session opened BECAUSE someone else broadcast them. Rebroadcasting one would put N clients in a
/// permanent round-robin over a flag that is already set everywhere. The local open flag is its
/// own latch, so a re-applied open is not an edge and never reaches this function in the first
/// place -- this filter covers the one case that is a genuine edge: the first time the link opens
/// a region here.
///
/// Reconnect replay needs nothing extra for the same reason: the receive stream re-dispatches every
/// held Lock on reconnect, but `open_on_received_name` edge-detects (flag CLEAR before, SET after),
/// so a replayed Lock produces no edge and `unlocked` is empty.
pub fn outbound(
    enabled: bool,
    unlocked: &[String],
    applied_from_link: &HashSet<String>,
) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    let mut seen: HashSet<&str> = HashSet::new();
    unlocked
        .iter()
        .filter(|r| !applied_from_link.contains(r.as_str()) && seen.insert(r.as_str()))
        .cloned()
        .collect()
}

/// Console/toast line for an applied sync open. ASCII ONLY -- this goes through the FMG path
/// (`every_toast_is_ascii`); an em-dash here draws as `?` in game.
pub fn sync_open_line(region: &str, source: &str) -> String {
    format!("Region Sync: {region} opened by {source}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn encode_round_trips_through_parse() {
        let v = encode_open("Alaric", "Liurnia", 1.0);
        let got = parse_open(&v, Some("Doopliss")).expect("foreign packet parses");
        assert_eq!(got.source, "Alaric");
        assert_eq!(got.region, "Liurnia");
    }

    #[test]
    fn own_broadcast_is_not_applied_back() {
        let v = encode_open("Alaric", "Liurnia", 1.0);
        assert_eq!(parse_open(&v, Some("Alaric")), None);
    }

    #[test]
    fn malformed_packets_are_skipped() {
        assert_eq!(parse_open(&json!({ "region": "Liurnia" }), None), None);
        assert_eq!(parse_open(&json!({ "source": "Alaric" }), None), None);
        assert_eq!(
            parse_open(&json!({ "source": "Alaric", "region": "" }), None),
            None
        );
    }

    #[test]
    fn disabled_broadcasts_nothing() {
        assert!(outbound(false, &["Liurnia".to_string()], &HashSet::new()).is_empty());
    }

    /// THE MOTIVATING CASE for the anti-echo: B opens Liurnia only because A broadcast it, so B
    /// must stay quiet or the two clients trade the same open forever.
    #[test]
    fn a_link_applied_open_is_not_rebroadcast() {
        let unlocked = vec!["Liurnia".to_string(), "Caelid".to_string()];
        let out = outbound(true, &unlocked, &set(&["Liurnia"]));
        assert_eq!(out, vec!["Caelid".to_string()]);
    }

    #[test]
    fn a_locally_opened_region_is_broadcast_once() {
        let unlocked = vec!["Caelid".to_string(), "Caelid".to_string()];
        assert_eq!(
            outbound(true, &unlocked, &HashSet::new()),
            vec!["Caelid".to_string()]
        );
    }

    #[test]
    fn line_is_ascii() {
        assert!(sync_open_line("Scadu Altus", "Doopliss").is_ascii());
    }
}
