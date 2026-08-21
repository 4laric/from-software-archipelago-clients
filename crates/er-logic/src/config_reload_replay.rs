//! Config hot-reload replay: a timeline of a human editing apconfig.json while the game runs.
//!
//! The two failure modes this pins are both "later" bugs -- they only exist across ticks:
//!   * RECONNECT STORM: `update_connection_info` SAVES the file, so a watcher that reacts to any
//!     change reacts to ITS OWN WRITE, forever.
//!   * TORN READ: a text editor's save is not atomic. Catch the file mid-write and it parses to empty
//!     fields; reconnecting to `url: ""` would drop a live session for nothing.

#![cfg(test)]

use crate::config_reload::{reload_action, ConnInfo, ReloadAction};

fn conn(url: &str, slot: &str) -> ConnInfo {
    ConnInfo {
        url: url.into(),
        slot: slot.into(),
        password: None,
    }
}

/// What the client is holding, and how many times it reconnected.
struct Sim {
    applied: ConnInfo,
    reconnects: Vec<ConnInfo>,
}

impl Sim {
    fn new(applied: ConnInfo) -> Self {
        Sim {
            applied,
            reconnects: vec![],
        }
    }

    /// One watcher tick against whatever the file currently says.
    fn tick(&mut self, on_disk: &ConnInfo) {
        match reload_action(&self.applied, on_disk) {
            ReloadAction::Ignore => {}
            ReloadAction::Reconnect(next) => {
                // Production applies it and SAVES -- so `applied` becomes the file's content, which is
                // exactly what makes the next tick a no-op instead of a storm.
                self.applied = next.clone();
                self.reconnects.push(next);
            }
        }
    }
}

/// The whole point: edit the server address, get exactly ONE reconnect.
#[test]
fn editing_the_url_reconnects_exactly_once() {
    let mut s = Sim::new(conn("localhost:38281", "Tester_A2"));
    let edited = conn("archipelago.gg:12345", "Tester_A2");

    s.tick(&edited); // the save lands
    s.tick(&edited); // watcher ticks again (our own saved file)
    s.tick(&edited); // ...and again
    s.tick(&edited);

    assert_eq!(
        s.reconnects,
        vec![edited],
        "one edit must produce ONE reconnect, not a storm"
    );
}

/// A torn read (editor mid-save) must never drop a live connection.
#[test]
fn a_half_written_file_is_ignored_not_connected_to() {
    let live = conn("localhost:38281", "Tester_A2");
    let mut s = Sim::new(live.clone());

    s.tick(&ConnInfo::default()); // file truncated to {} mid-save
    s.tick(&conn("", "Tester_A2")); // url written after slot
    s.tick(&conn("archipelago.gg:12345", "")); // slot not written yet

    assert!(s.reconnects.is_empty(), "a torn read must NOT reconnect");
    assert_eq!(s.applied, live, "and must not disturb the live connection");

    // The complete save arrives -> now, and only now, reconnect.
    let done = conn("archipelago.gg:12345", "Tester_A2");
    s.tick(&done);
    assert_eq!(s.reconnects, vec![done]);
}

/// Changing the slot (same server) is a real change.
#[test]
fn changing_the_slot_reconnects() {
    let mut s = Sim::new(conn("localhost:38281", "Tester_A2"));
    let other = conn("localhost:38281", "Tester_A3");
    s.tick(&other);
    assert_eq!(s.reconnects, vec![other]);
}

/// Password-only change still reconnects (it is part of the credentials).
#[test]
fn a_password_change_reconnects() {
    let mut s = Sim::new(conn("localhost:38281", "Tester_A2"));
    let with_pw = ConnInfo {
        password: Some("hunter2".into()),
        ..conn("localhost:38281", "Tester_A2")
    };
    s.tick(&with_pw);
    assert_eq!(s.reconnects.len(), 1);
    assert_eq!(s.applied.password.as_deref(), Some("hunter2"));
}

/// Rewriting the SAME content (editor "save" with no edit) must be inert.
#[test]
fn saving_without_changing_anything_does_nothing() {
    let live = conn("localhost:38281", "Tester_A2");
    let mut s = Sim::new(live.clone());
    for _ in 0..5 {
        s.tick(&live);
    }
    assert!(
        s.reconnects.is_empty(),
        "an idempotent save must not reconnect"
    );
}

/// A whole realistic session: connect, mis-type, fix it, switch slot. One reconnect per real change.
#[test]
fn a_realistic_editing_session_reconnects_once_per_real_change() {
    let mut s = Sim::new(conn("localhost:38281", "Tester_A2"));
    let typo = conn("archipelago.gg:1234", "Tester_A2");
    let fixed = conn("archipelago.gg:12345", "Tester_A2");
    let slot2 = conn("archipelago.gg:12345", "Tester_A3");

    s.tick(&ConnInfo::default()); // torn
    s.tick(&typo);
    s.tick(&typo); // our own save echoes back
    s.tick(&fixed);
    s.tick(&fixed);
    s.tick(&slot2);
    s.tick(&slot2);

    assert_eq!(
        s.reconnects,
        vec![typo, fixed, slot2],
        "exactly one reconnect per REAL change -- no storm, no torn-read reconnect"
    );
}

// ---------------------------------------------------------------------------------------------
// Live probe toggles (client#166) -- the same timeline discipline for the `probes` object
// ---------------------------------------------------------------------------------------------

use crate::config_reload::{probe_reload_action, ProbeAction, ProbeMap};

fn probes(pairs: &[(&str, bool)]) -> ProbeMap {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// What the client has installed, and how many times a probe set was (re)applied.
struct ProbeSim {
    applied: ProbeMap,
    applies: Vec<ProbeMap>,
}

impl ProbeSim {
    fn new(applied: ProbeMap) -> Self {
        ProbeSim {
            applied,
            applies: vec![],
        }
    }

    /// One watcher tick against the probes object currently on disk. Production folds the
    /// applied map into the in-memory Config as part of Apply, so the client's own later
    /// `save()` writes the SAME probes back -- which is exactly what makes the next tick a
    /// no-op instead of a storm (the echo trap, probe edition).
    fn tick(&mut self, on_disk: &ProbeMap) {
        match probe_reload_action(&self.applied, on_disk) {
            ProbeAction::Ignore => {}
            ProbeAction::Apply(next) => {
                self.applied = next.clone();
                self.applies.push(next);
            }
        }
    }
}

/// THE MOTIVATING CASE (#166): bobler edits one probe line mid-session, alt-tabs back, and the
/// probe is on -- applied EXACTLY ONCE, with every later tick (including the echo of the save the
/// apply itself caused) a no-op.
#[test]
fn editing_a_probe_applies_exactly_once() {
    let mut s = ProbeSim::new(probes(&[]));
    let on = probes(&[("trap_feel", true)]);

    s.tick(&on); // the edit lands -> apply
    s.tick(&on); // watcher ticks again (our own saved file)
    s.tick(&on);
    s.tick(&on);

    assert_eq!(s.applies, vec![on], "one edit must produce ONE apply");
}

/// BOTH directions (#166's verification note): setting `false` on an installed probe is an apply
/// too, or "turn it off again" would need the restart this feature exists to remove.
#[test]
fn turning_a_probe_back_off_applies() {
    let mut s = ProbeSim::new(probes(&[("trap_feel", true)]));
    let off = probes(&[("trap_feel", false)]);
    s.tick(&off);
    assert_eq!(s.applies, vec![off]);
    assert_eq!(s.applied, probes(&[("trap_feel", false)]));
}

/// Deleting the whole `probes` object means "all off": an empty map is a meaningful edit, NOT a
/// torn read (a torn write never parses, so it never reaches this predicate at all).
#[test]
fn deleting_the_probes_object_turns_everything_off() {
    let mut s = ProbeSim::new(probes(&[("esd", true), ("hover", true)]));
    s.tick(&probes(&[]));
    assert_eq!(s.applies, vec![probes(&[])]);
    assert!(s.applied.is_empty());
}

/// A playtester's UNKNOWN probe key (a typo, or a newer client's name) is data, and toggling it
/// still applies -- the watcher compares maps, it does not know the probe roster. `log_active`
/// is what makes the typo answerable, and the re-armed announcement covers this change too.
#[test]
fn an_unknown_key_change_still_applies() {
    let mut s = ProbeSim::new(probes(&[]));
    let typo = probes(&[("esd_probe", true)]); // the real key is "esd"
    s.tick(&typo);
    assert_eq!(s.applies, vec![typo]);
}

/// The echo trap, stated as a storm test: apply -> our save writes the same map -> the next
/// hundred ticks must be silent. (The replay only models this correctly because Apply updates
/// `applied`; production's twin is folding the map into the in-memory Config before any save.)
#[test]
fn our_own_save_after_an_apply_echoes_to_nothing() {
    let mut s = ProbeSim::new(probes(&[]));
    let on = probes(&[("esd", true)]);
    s.tick(&on);
    for _ in 0..100 {
        s.tick(&on); // what our own save() wrote back
    }
    assert_eq!(
        s.applies.len(),
        1,
        "the echo of our own save must not re-apply"
    );
}
