//! `ownership` — may a grant path stand down, and is anyone actually going to deliver?
//!
//! # The bug this exists for (I3, 2026-08-01)
//!
//! The strangler cutover let the reconciler take over grant classes one at a time, and `core.rs`
//! skips its OWN handler for any class the reconciler owns, so the two never both mutate. The
//! predicate for "owns" was a pure CONFIG read:
//!
//! ```text
//! owns_goods() := !dry_run() && apply_classes().goods
//! ```
//!
//! Config says who *should* deliver. It cannot say whether that owner exists. `reconcile_io::tick()`
//! returns early when the session is REFUSED, and the `Driver` is only built at the first stable
//! in-world tick — which never arrives if the inventory pointer is never captured. In either state
//! `owns_goods()` still answered `true`, so `core.rs` skipped its grant AND the H3 watermark hold
//! that protects it, the receive cursor advanced on faith, and `write_save()` made the loss durable.
//! A player reported exactly this on 2026-08-01: checks still SENT (the report paths are gated only
//! on `is_refused`), nothing ever arrived again, and a fresh character got no start item either.
//!
//! # The invariant
//!
//! **Config may choose WHO delivers; it may never assert THAT delivery happened.** A class is owned
//! only when an owner is configured, armed, and not refused. Everything else falls back to the old
//! handler, which carries its own verified-placement hold — so the failure mode becomes a held
//! cursor and a retry instead of a silent, persisted loss.
//!
//! Pure: no Windows, no globals, no I/O. `reconcile_io` supplies the live [`OwnerState`] and asks.

use crate::reconcile::ApplyClasses;

/// The three grant classes the strangler cutover moves one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Flags,
    Goods,
    Ledger,
}

/// Everything the ownership question depends on, gathered at one instant.
///
/// `armed` and `refused` are the two facts the old config-only predicate was blind to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerState {
    /// What `RECONCILE_APPLY` names.
    pub configured: ApplyClasses,
    /// `RECONCILE_DRYRUN=1` — plan only, apply nothing.
    pub dry_run: bool,
    /// A `Driver` exists: `reconcile_io::init` ran and built a reconciler for this session.
    pub armed: bool,
    /// The marker identity guard refused at init, or `disarm_if_identity_moved` disarmed mid-session.
    /// A refused session's `tick()` returns immediately, so a refused owner delivers nothing.
    pub refused: bool,
}

/// Why delivery is suspended — the reason a player is owed on screen (I4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suspended {
    /// Identity guard refused/disarmed. `reconcile_io::refusal_toast` already owns this message,
    /// which is why [`OwnerState::suspended`] reports it but the caller may defer to that text.
    Refused,
    /// Configured to deliver, but no `Driver` was ever built. The silent state that cost the
    /// 2026-08-01 report: nothing is coming, and before I3 nothing said so.
    NeverArmed,
}

/// How long a configured-but-unarmed session may run before it owes the player a notice.
///
/// Arming legitimately takes a moment: `init` waits for a stable in-world tick, and a slow load or
/// a long menu sit is normal. Sixty seconds of actual in-world time is far past that and safely
/// short of a play session, so the notice means "this is stuck", not "this is loading".
pub const NEVER_ARMED_GRACE_MS: u64 = 60_000;

impl OwnerState {
    /// The classes the reconciler will ACTUALLY apply. Empty unless an owner is configured, armed
    /// and not refused — the whole of I3 in one function.
    pub fn effective(&self) -> ApplyClasses {
        if self.dry_run || !self.armed || self.refused {
            return ApplyClasses::NONE;
        }
        self.configured
    }

    /// Does the reconciler own `class` — i.e. may `core.rs` skip its own handler for it?
    pub fn owns(&self, class: Class) -> bool {
        let c = self.effective();
        match class {
            Class::Flags => c.flags,
            Class::Goods => c.goods,
            Class::Ledger => c.ledger,
        }
    }

    /// Is an owner configured but not delivering? `None` when the session is healthy, or when
    /// nothing was configured to deliver in the first place (baseline/dry-run are deliberate modes,
    /// not faults).
    ///
    /// `in_world_ms` is how long this session has been in-world; the never-armed verdict waits out
    /// [`NEVER_ARMED_GRACE_MS`] so a normal load screen is never reported as a fault.
    pub fn suspended(&self, in_world_ms: u64) -> Option<Suspended> {
        let c = self.configured;
        if self.dry_run || !(c.flags || c.goods || c.ledger) {
            return None; // deliberate mode, not a fault
        }
        if self.refused {
            return Some(Suspended::Refused);
        }
        if !self.armed && in_world_ms >= NEVER_ARMED_GRACE_MS {
            return Some(Suspended::NeverArmed);
        }
        None
    }
}

/// The on-screen notice owed for a [`Suspended`] state. ASCII ONLY — this goes through the FMG path
/// (`every_toast_is_ascii`); an em-dash draws as `?` in game.
pub fn suspended_toast(s: Suspended) -> &'static str {
    match s {
        Suspended::Refused => {
            "AP: this save does not match the connected room. Items are NOT arriving. \
             Reconnect the right save, or restart the game."
        }
        Suspended::NeverArmed => {
            "AP: item delivery has not started. Pick up any item to wake it, or check for \
             another mod hooking item pickups."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy session: configured for everything, armed, not refused.
    fn healthy() -> OwnerState {
        OwnerState {
            configured: ApplyClasses::ALL,
            dry_run: false,
            armed: true,
            refused: false,
        }
    }

    #[test]
    fn healthy_session_owns_every_configured_class() {
        let s = healthy();
        assert!(s.owns(Class::Flags));
        assert!(s.owns(Class::Goods));
        assert!(s.owns(Class::Ledger));
        assert_eq!(s.suspended(10 * NEVER_ARMED_GRACE_MS), None);
    }

    /// THE REGRESSION. Configured to own everything but never armed: the old predicate said "owned"
    /// and `core.rs` stood down, so nothing granted and the cursor advanced anyway.
    #[test]
    fn configured_but_unarmed_owns_nothing() {
        let s = OwnerState {
            armed: false,
            ..healthy()
        };
        assert!(!s.owns(Class::Goods), "an unarmed driver cannot own goods");
        assert!(
            !s.owns(Class::Ledger),
            "an unarmed driver cannot own the ledger"
        );
        assert!(!s.owns(Class::Flags));
        assert_eq!(s.effective(), ApplyClasses::NONE);
    }

    /// A refused session's `tick()` returns immediately, so it delivers nothing either.
    #[test]
    fn refused_owns_nothing_even_though_armed() {
        let s = OwnerState {
            refused: true,
            ..healthy()
        };
        assert_eq!(s.effective(), ApplyClasses::NONE);
        assert!(!s.owns(Class::Goods));
    }

    #[test]
    fn dry_run_owns_nothing() {
        let s = OwnerState {
            dry_run: true,
            ..healthy()
        };
        assert_eq!(s.effective(), ApplyClasses::NONE);
    }

    /// Ownership is per class: a partial cutover still hands the un-owned classes to the old path.
    #[test]
    fn partial_config_is_respected_when_armed() {
        let s = OwnerState {
            configured: ApplyClasses {
                flags: true,
                goods: false,
                ledger: false,
            },
            ..healthy()
        };
        assert!(s.owns(Class::Flags));
        assert!(!s.owns(Class::Goods));
    }

    /// A normal load screen must not raise the notice.
    #[test]
    fn unarmed_within_grace_is_not_yet_a_fault() {
        let s = OwnerState {
            armed: false,
            ..healthy()
        };
        assert_eq!(s.suspended(0), None);
        assert_eq!(s.suspended(NEVER_ARMED_GRACE_MS - 1), None);
        assert_eq!(
            s.suspended(NEVER_ARMED_GRACE_MS),
            Some(Suspended::NeverArmed)
        );
    }

    /// Refusal outranks never-armed: a refused session never arms, and the refusal is the actionable
    /// message.
    #[test]
    fn refused_outranks_never_armed() {
        let s = OwnerState {
            armed: false,
            refused: true,
            ..healthy()
        };
        assert_eq!(s.suspended(0), Some(Suspended::Refused));
    }

    /// Baseline and dry-run own nothing BY DESIGN — they are modes, not faults, and must stay silent.
    #[test]
    fn deliberate_modes_are_never_reported_as_suspended() {
        let baseline = OwnerState {
            configured: ApplyClasses::NONE,
            armed: false,
            ..healthy()
        };
        assert_eq!(baseline.suspended(10 * NEVER_ARMED_GRACE_MS), None);

        let dry = OwnerState {
            dry_run: true,
            armed: false,
            ..healthy()
        };
        assert_eq!(dry.suspended(10 * NEVER_ARMED_GRACE_MS), None);
    }

    #[test]
    fn every_toast_is_ascii() {
        for s in [Suspended::Refused, Suspended::NeverArmed] {
            let t = suspended_toast(s);
            assert!(t.is_ascii(), "toast must be ASCII (FMG path): {t:?}");
        }
    }
}
