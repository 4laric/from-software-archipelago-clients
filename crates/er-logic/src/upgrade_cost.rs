//! Pure flatten-regular-upgrades decision logic (host-testable, no eldenring/param deps).
//!
//! The `eldenring-archipelago::upgrade_cost` module applies these against the LIVE
//! `EquipMtrlSetParam` table; extracting them here makes the two hazardous properties replay-testable
//! on any host (see `upgrade_cost_replay`):
//!   * LOWER-ONLY clamp: never raises a step above vanilla; leaves unused / non-regular / already-low
//!     slots untouched (reconnect-safe, no cost inflation).
//!   * RE-ARM latch: the game reloads regulation.bin (params -> vanilla) on every load, so a reconnect
//!     MUST re-arm and re-apply or the flatten is silently lost. This is the ship risk being guarded.

/// Regular (non-somber) upgrade materials: Smithing Stone [1]-[8] (EquipParamGoods 10100-10107) +
/// Ancient Dragon Smithing Stone (10140). Somber stones (10160-10168, 10200) are EXCLUDED so somber
/// weapons keep their vanilla curve.
pub const REGULAR_STONE_IDS: &[i32] = &[
    10100, 10101, 10102, 10103, 10104, 10105, 10106, 10107, 10140,
];

/// Upper clamp on the resolved option value (a regular step never legitimately costs more).
pub const MAX_CAP: i32 = 6;

/// `applied_at` sentinel meaning "re-armed, not yet applied this connection".
pub const SENTINEL: i32 = -1;

pub fn is_regular_stone(id: i32) -> bool {
    REGULAR_STONE_IDS.contains(&id)
}

/// Per-slot decision: for a material slot `(material_id, cur)` and resolved cap `N`, return
/// `Some(new_count)` iff it must be LOWERED, else `None` (leave as-is). Off (`cap <= 0`) or an
/// out-of-range cap is identity. Unused slots (`cur < 0`), non-regular stones, and steps already at
/// or below the cap are never touched -- lower-only, so re-running is idempotent and never inflates.
pub fn clamp_count(material_id: i32, cur: i8, cap: i32) -> Option<i8> {
    if cap <= 0 || cap > MAX_CAP {
        return None;
    }
    let cap = cap as i8;
    if is_regular_stone(material_id) && cur > cap {
        Some(cap)
    } else {
        None
    }
}

/// One-shot re-arm latch for the in-world apply. Mirrors the client's `FLAT_N` / `APPLIED` atomics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlattenLatch {
    /// Resolved stones/level cap (0 = off), clamped to `[0, MAX_CAP]`.
    pub cap: i32,
    /// Cap last applied to the live param table, or `SENTINEL` when re-armed and not yet applied.
    pub applied_at: i32,
}

impl Default for FlattenLatch {
    fn default() -> Self {
        FlattenLatch { cap: 0, applied_at: SENTINEL }
    }
}

impl FlattenLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called on every CONNECT (and reconnect): (re)set the cap and RE-ARM. Because the game reloads
    /// regulation to vanilla on load, a reconnect must re-arm here or the clamp is lost -- this is the
    /// crux of the reconnect-safety guarantee.
    pub fn set(&mut self, cap: i32) {
        self.cap = cap.clamp(0, MAX_CAP);
        self.applied_at = SENTINEL;
    }

    /// Should the in-world tick apply now? Enabled AND not already applied at the current cap.
    pub fn should_apply(&self) -> bool {
        self.cap > 0 && self.applied_at != self.cap
    }

    /// Record that the clamp was applied at the current cap (idempotent until the next `set`).
    pub fn mark_applied(&mut self) {
        self.applied_at = self.cap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_is_lower_only_and_regular_only() {
        // regular stone above cap -> lowered; at/below cap -> untouched; unused (-1) -> untouched.
        assert_eq!(clamp_count(10100, 6, 3), Some(3));
        assert_eq!(clamp_count(10100, 2, 3), None);
        assert_eq!(clamp_count(10100, 3, 3), None);
        assert_eq!(clamp_count(10100, -1, 3), None);
        // somber stone never touched even above cap.
        assert_eq!(clamp_count(10160, 6, 3), None);
        // off / out-of-range cap = identity.
        assert_eq!(clamp_count(10100, 6, 0), None);
        assert_eq!(clamp_count(10100, 6, MAX_CAP + 1), None);
    }

    #[test]
    fn latch_arms_on_set_and_is_one_shot() {
        let mut l = FlattenLatch::new();
        assert!(!l.should_apply(), "off by default");
        l.set(3);
        assert!(l.should_apply(), "armed after set");
        l.mark_applied();
        assert!(!l.should_apply(), "one-shot: no re-apply until re-armed");
        // re-set (reconnect) re-arms even at the SAME cap.
        l.set(3);
        assert!(l.should_apply(), "reconnect re-arms at the same cap");
        // set(0) disables.
        l.set(0);
        assert!(!l.should_apply());
        // cap is clamped to [0, MAX_CAP].
        l.set(999);
        assert_eq!(l.cap, MAX_CAP);
    }
}
