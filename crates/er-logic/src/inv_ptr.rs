//! `inv_ptr` — is the cached inventory pointer still the game's?
//!
//! ## The crash this exists for (2026-07-24, symbolized)
//!
//! `detour::grant_full_id` grants by calling the game's AddItemFunc with a raw inventory pointer
//! captured once — from the player's first real pickup (`add_item_detour`), or primed from a static
//! slot — and then held in `LAST_INVENTORY` **forever**. The only guard was `inv < 0x10000`, which
//! catches null and nothing else.
//!
//! The game frees and rebuilds that object across a map load. After any load, the cached value
//! points at freed memory, and the next grant hands it to the game, which dereferences it:
//!
//! ```text
//! eldenring.exe+0x560714  ACCESS_VIOLATION read at 0x1ffa585e148   <- the game, using OUR pointer
//!   <- eldenring_archipelago::detour::grant_full_id
//!   <- er_logic::reconcile::Reconciler::tick_with_classes
//!   <- eldenring_archipelago::reconcile_io::tick
//!   <- Core::classify_received  <- Core::update  <- the game's RecurringTask
//! ```
//!
//! A plausible-looking heap address, not a null — the signature of a freed block, not an
//! uninitialised one. Both observed CTDs landed seconds after a boss sweep, because a sweep hands
//! the server a batch of checks, the server echoes a batch of items, and the reconciler then calls
//! this path many times in a row: more grants after a load = more chances to use the stale pointer.
//! (Volume alone is NOT the cause — the 1-2k-check mass-grant test passed. Timing is.)
//!
//! ## The rule
//!
//! A captured pointer belongs to the WORLD EPOCH it was captured in. Every in-world edge (load,
//! save-load, warp arrival, respawn) bumps the epoch, and a pointer from an older epoch is dead —
//! not "probably fine", dead. Re-priming is cheap and already implemented; using a stale pointer
//! is a native crash we cannot catch.

/// May a pointer captured in `captured_epoch` be used now, in `world_epoch`?
///
/// `ptr` is the raw value; anything below `MIN_PLAUSIBLE` is null-ish and never usable. The epoch
/// test is the actual fix: same epoch or nothing.
pub fn usable(ptr: usize, captured_epoch: u64, world_epoch: u64) -> bool {
    ptr >= MIN_PLAUSIBLE && captured_epoch == world_epoch
}

/// Below this a pointer is null / obviously bogus. (The pre-fix guard was ONLY this test.)
pub const MIN_PLAUSIBLE: usize = 0x10000;

#[cfg(test)]
mod replay {
    use super::*;

    #[derive(Clone, Copy, PartialEq)]
    enum Policy {
        /// Pre-fix: captured once, trusted forever; only the null-ish check applies.
        TrustForever,
        /// The fix: a pointer is only valid within the epoch it was captured in.
        EpochScoped,
    }

    enum Ev {
        /// A real pickup (or the static prime) captures the live inventory pointer.
        Capture(usize),
        /// A map load: the game frees/rebuilds the inventory object. Any pointer captured before
        /// this now refers to freed memory.
        MapLoad,
        /// The reconciler grants an item through the cached pointer.
        Grant,
    }

    struct Run {
        /// Grants that went through a pointer belonging to an older epoch: each one is a
        /// use-after-free handed to the game, i.e. a native crash.
        stale_uses: u32,
        /// Grants that were correctly deferred (caller retries next tick).
        deferred: u32,
        /// Grants delivered through a live pointer.
        delivered: u32,
    }

    fn replay(events: &[Ev], policy: Policy) -> Run {
        let (mut ptr, mut captured, mut epoch) = (0usize, 0u64, 0u64);
        // Which epoch the game's real object belongs to -- the oracle the client cannot see.
        let mut live_epoch = 0u64;
        let mut r = Run {
            stale_uses: 0,
            deferred: 0,
            delivered: 0,
        };
        for ev in events {
            match ev {
                Ev::Capture(p) => {
                    ptr = *p;
                    captured = epoch;
                }
                Ev::MapLoad => {
                    epoch += 1;
                    live_epoch += 1;
                }
                Ev::Grant => {
                    let ok = match policy {
                        Policy::TrustForever => ptr >= MIN_PLAUSIBLE,
                        Policy::EpochScoped => usable(ptr, captured, epoch),
                    };
                    if !ok {
                        r.deferred += 1;
                    } else if captured != live_epoch {
                        r.stale_uses += 1; // handed freed memory to the game
                    } else {
                        r.delivered += 1;
                    }
                }
            }
        }
        r
    }

    #[test]
    fn a_grant_after_a_map_load_uses_freed_memory_pre_fix() {
        // THE CRASH: capture, load, grant. One line of difference between a delivery and an AV.
        let timeline = [Ev::Capture(0x1ffa585e000), Ev::MapLoad, Ev::Grant];
        let old = replay(&timeline, Policy::TrustForever);
        assert_eq!(
            old.stale_uses, 1,
            "pre-fix: the grant goes through a pointer the game already freed"
        );
        let new = replay(&timeline, Policy::EpochScoped);
        assert_eq!(new.stale_uses, 0, "post-fix: never handed a stale pointer");
        assert_eq!(new.deferred, 1, "deferred instead -- caller retries after a re-prime");
    }

    #[test]
    fn a_burst_after_a_load_is_a_burst_of_use_after_free() {
        // Why the CTDs cluster on boss sweeps: a sweep -> batch of checks -> batch of echoed items
        // -> many grants in a row. Every one of them is a roll of the same dice.
        let mut timeline = vec![Ev::Capture(0x1ffa585e000), Ev::MapLoad];
        timeline.extend((0..10).map(|_| Ev::Grant));
        assert_eq!(replay(&timeline, Policy::TrustForever).stale_uses, 10);
        assert_eq!(replay(&timeline, Policy::EpochScoped).stale_uses, 0);
    }

    #[test]
    fn re_priming_after_the_load_restores_delivery() {
        // The fix must not strand grants: the next in-world tick re-primes (static slot or the
        // player's next pickup) and delivery resumes in the same epoch.
        let timeline = [
            Ev::Capture(0x1ffa585e000),
            Ev::MapLoad,
            Ev::Grant,
            Ev::Capture(0x2000000000),
            Ev::Grant,
        ];
        let r = replay(&timeline, Policy::EpochScoped);
        assert_eq!(r.deferred, 1, "the one before the re-prime");
        assert_eq!(r.delivered, 1, "and the one after it lands");
        assert_eq!(r.stale_uses, 0);
    }

    #[test]
    fn grants_within_one_epoch_are_unaffected() {
        // No behaviour change in the common case: capture once, grant all session, no load.
        let mut timeline = vec![Ev::Capture(0x1ffa585e000)];
        timeline.extend((0..5).map(|_| Ev::Grant));
        let r = replay(&timeline, Policy::EpochScoped);
        assert_eq!(r.delivered, 5);
        assert_eq!(r.deferred, 0);
    }

    #[test]
    fn a_null_pointer_is_still_refused() {
        assert!(!usable(0, 0, 0));
        assert!(!usable(0x100, 0, 0), "below MIN_PLAUSIBLE");
        assert!(usable(0x10000, 3, 3));
        assert!(!usable(0x10000, 2, 3), "right pointer, wrong epoch");
    }
}
