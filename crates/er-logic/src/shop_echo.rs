//! `shop_echo` — eligibility of a check for shop_sell's native-rewrite + ECHO-DEDUP, plus the
//! timeline replay of the START-GRANT collision that LOST three AP items (2026-07-24 playtest).
//!
//! ECHO-DEDUP's whole contract is an implication: *stock flag set ⇒ the rewritten row's native
//! sale delivered the reward*, so the echo grant may be skipped. That implication holds only if
//! NOTHING but the purchase can set the flag. But the near-spawn unique key items (physick 60020,
//! bell 60110, whetstone 60130, whistle 60100) use the game's vanilla OBTAINED-flag as their
//! check-detection flag — and the CLIENT ITSELF sets those flags outside any purchase:
//!
//!   * `core.rs` unique-start-grant block (slot_data `uniqueStartGrants`): grants the vanilla
//!     goods and sets the obtained-flag at connect ([`crate::unique_grants`]);
//!   * `keyitems.rs` acquire-flag tables: sets the same flags when the item is RECEIVED from
//!     the pool.
//!
//! Reproduced failure: shop_sell::run() armed ECHO-DEDUP for locations 7770011/12/13 (their
//! flags 60020/60110/60130 match live ShopLineupParam fallback rows), the unique start grant
//! then set those flags, the flag-poll sent the checks, and the echo of the AP rewards was
//! skipped as "sold natively at purchase" — vanilla in the bag, AP items never granted.
//!
//! The fix: a check whose stock flag is CLIENT-SETTABLE is simply NOT ELIGIBLE for the native
//! rewrite or the echo-arm ([`echo_dedup_eligible`]). Its shop row (if any) keeps selling the
//! vanilla ware and its echo always grants — the pre-ECHO-DEDUP behaviour, which never loses an
//! item. Both must be skipped TOGETHER: arming without rewriting loses the AP item (this bug),
//! rewriting without arming double-grants on a genuine purchase. `shop_sell::run()` must CALL
//! this predicate (CONTRIBUTING: a green predicate with no production caller is a spec, not a
//! fix); core.rs builds the set from `uniqueStartGrants` + the keyitems acquire tables.
//!
//! # The SECOND breaker: PARAM REVERT (same playtest, session 2, 15:45–15:53)
//!
//! The implication also fails when the ROW stops selling the reward. A map load streams params
//! back in and reverts runtime param writes — the exact mechanism behind the 2026-07-21 DLC
//! "vanilla ware leaks" bug, whose fix re-arms `check_lots`/`enemy_drops` on the in-world edge
//! (core.rs) but NOT `shop_sell`. Reproduced: `rewrote 571 own-world slot(s)` at 15:37:04, four
//! load edges (`check-lots: wrote …` re-fires), then Kalé purchases at 15:45 sold the VANILLA
//! wares (screenshot: "Note: Waypoint Ruins — No. Held 1") while the echoes of the real rewards
//! were skipped as "sold natively at purchase". Every one of the seed's ~548 armed shop checks
//! bought after the first reload lost its AP item this way.
//!
//! Two-part fix, both halves required:
//!   * shop_sell joins the in-world-edge re-arm (core.rs `reset()`s it beside check_lots), so
//!     rows go back to natively selling their reward after every load;
//!   * `echo_skip` stops trusting the flag alone: [`echo_skip_decision`] skips the echo grant
//!     iff the flag is set AND the recorded row STILL sells the reward at echo time. A reverted
//!     (or unreadable) row cannot have delivered the reward since the last rewrite, so the echo
//!     grants. Bias is deliberate: when delivery cannot be proven, prefer a rare duplicate over
//!     a lost item (an AP item lost past the watermark is unrecoverable; a dupe is junk).

use std::collections::HashSet;

/// May shop_sell natively rewrite this check's row AND arm ECHO-DEDUP for it? `false` when the
/// check's stock/detection flag is one the client itself can set outside a purchase (unique
/// start grants, keyitems acquire flags) — flag-set then no longer proves a native sale, so the
/// row must stay vanilla and the echo must always grant.
pub fn echo_dedup_eligible(stock_flag: u32, client_settable_flags: &HashSet<u32>) -> bool {
    !client_settable_flags.contains(&stock_flag)
}

/// Should the echo grant for an ARMED check be skipped? `flag_set` is the live stock-flag read;
/// `row_still_sells_reward` is the live re-read of the recorded ShopLineupParam row — true iff
/// its (equipId, equipType) still equal the reward shop_sell wrote (false too when the repo/row
/// cannot be read: unprovable delivery must GRANT, never skip). Skip only when BOTH hold — a set
/// flag on a reverted row means the purchase delivered the VANILLA ware (param stream-in undid
/// the rewrite), so the echo is the only chance the AP item has.
pub fn echo_skip_decision(flag_set: bool, row_still_sells_reward: bool) -> bool {
    flag_set && row_still_sells_reward
}

#[cfg(test)]
mod replay {
    use super::*;

    /// Real ids from the failing 2026-07-24 log: check flag 60130 ("Whetstone Knife - near
    /// Gatefront", AP location 7770013), vanilla ware goods 8590, AP reward Hero's Rune [5].
    const START_GRANT_FLAG: u32 = 60130;
    /// An ordinary shop-check stock flag (fixedShopStart group, EVENT-FLAG-SPACE.md) — set by
    /// the purchase and by NOTHING else.
    const ORDINARY_FLAG: u32 = 100400;

    /// Arming policy under test. The failing-without / passing-with pair below toggles this.
    #[derive(Clone, Copy, PartialEq)]
    enum Policy {
        /// Pre-fix: every open configured check whose flag matches a live row is rewritten
        /// and echo-armed, unconditionally.
        ArmAll,
        /// The fix: [`echo_dedup_eligible`] gates BOTH the rewrite and the echo-arm.
        ExemptClientSettable,
    }

    /// The echo-skip rule at EchoArrive — the second fix axis.
    #[derive(Clone, Copy, PartialEq)]
    enum SkipRule {
        /// Pre-fix `echo_skip`: armed + flag set. Trusts that the rewrite is still live.
        FlagOnly,
        /// The fix: armed + [`echo_skip_decision`] (flag set AND the row STILL sells the
        /// reward at echo time).
        FlagAndLiveRow,
    }

    /// The frames that matter for this bug.
    enum Ev {
        /// shop_sell::run(): for the (open) check row — per policy, rewrite the row to sell the
        /// AP reward natively and record the echo-arm.
        ShopSellRun,
        /// The unique start grant fires: vanilla goods granted, obtained-flag SET (no purchase).
        UniqueStartGrant,
        /// The pool/receive path delivers the vanilla item from another check; keyitems.rs sets
        /// the same obtained-flag (no purchase).
        KeyItemReceiveSetsFlag,
        /// The player buys the shop row: whatever the row currently sells lands in the bag, and
        /// the stock flag sets.
        Purchase,
        /// A map load streams params back in: every runtime ShopLineupParam rewrite REVERTS to
        /// the vanilla ware. Client state (ECHO_SKIP, DONE latch, flags) survives — that
        /// asymmetry IS the second bug surface (the 2026-07-21 check_lots leak, shop_sell
        /// edition).
        ParamRevert,
        /// The in-world-edge re-arm: shop_sell::reset() + the next tick's run(). Open checks
        /// are re-rewritten + re-armed; a completed check (flag set) is NOT re-armed
        /// (shop_sell.rs "checks already completed are NOT recorded").
        Rearm,
        /// The server echo of THIS check's AP reward arrives; the receive loop skips the grant
        /// per the SkipRule (shop_sell::echo_skip).
        EchoArrive,
    }

    /// What the player ended up with — `ap_reward` is the loss detector.
    struct Outcome {
        /// Copies of the AP reward delivered (native sale of a rewritten row, or echo grant).
        ap_reward: u32,
        /// Copies of the vanilla ware delivered (start grant, or native sale of a vanilla row).
        vanilla: u32,
        flag_set: bool,
    }

    fn replay(
        events: &[Ev],
        stock_flag: u32,
        policy: Policy,
        rule: SkipRule,
        client_settable: &HashSet<u32>,
    ) -> Outcome {
        let mut out = Outcome {
            ap_reward: 0,
            vanilla: 0,
            flag_set: false,
        };
        let mut row_sells_reward = false; // rewritten?
        let mut echo_armed = false; // in ECHO_SKIP?
        for ev in events {
            match ev {
                Ev::ShopSellRun => {
                    let eligible = match policy {
                        Policy::ArmAll => true,
                        Policy::ExemptClientSettable => {
                            echo_dedup_eligible(stock_flag, client_settable)
                        }
                    };
                    // Rewrite and arm travel TOGETHER (see module doc); checks already
                    // completed are never armed (shop_sell.rs:240).
                    if eligible && !out.flag_set {
                        row_sells_reward = true;
                        echo_armed = true;
                    }
                }
                Ev::UniqueStartGrant => {
                    // core.rs unique-grant block: vanilla goods + flag, NO purchase.
                    if crate::unique_grants::unique_grant_action(out.flag_set) {
                        out.vanilla += 1;
                        out.flag_set = true;
                    }
                }
                Ev::KeyItemReceiveSetsFlag => out.flag_set = true, // keyitems.rs acquire flag
                Ev::Purchase => {
                    if row_sells_reward {
                        out.ap_reward += 1;
                    } else {
                        out.vanilla += 1;
                    }
                    out.flag_set = true;
                }
                Ev::ParamRevert => row_sells_reward = false, // stream-in; ECHO_SKIP survives
                Ev::Rearm => {
                    // reset() + next run(): re-rewrite open checks; completed checks (flag
                    // set) fall out of the rebuilt ECHO_SKIP.
                    if out.flag_set {
                        echo_armed = false;
                    } else {
                        let eligible = policy == Policy::ArmAll
                            || echo_dedup_eligible(stock_flag, client_settable);
                        if eligible {
                            row_sells_reward = true;
                            echo_armed = true;
                        }
                    }
                }
                Ev::EchoArrive => {
                    // core.rs receive loop: sender==self && echo_skip(loc) -> SkipNativelySold.
                    let skip = echo_armed
                        && match rule {
                            SkipRule::FlagOnly => out.flag_set,
                            SkipRule::FlagAndLiveRow => {
                                echo_skip_decision(out.flag_set, row_sells_reward)
                            }
                        };
                    if !skip {
                        out.ap_reward += 1;
                    }
                }
            }
        }
        out
    }

    fn exempt() -> HashSet<u32> {
        // uniqueStartGrants obtained-flags ∪ keyitems acquire flags (production set built by
        // core.rs from slot_data + keyitems::all_acquire_flags()).
        [60020u32, 60100, 60110, 60130].into_iter().collect()
    }

    #[test]
    fn start_grant_collision_loses_the_ap_item_pre_fix_and_delivers_it_post_fix() {
        // THE BUG, exactly as logged 2026-07-24: run() arms the open check, the unique start
        // grant sets 60130 with the VANILLA goods, the echo of Hero's Rune [5] arrives and is
        // skipped as "sold natively at purchase" -- no purchase ever happened.
        let timeline = [Ev::ShopSellRun, Ev::UniqueStartGrant, Ev::EchoArrive];
        let old = replay(&timeline, START_GRANT_FLAG, Policy::ArmAll, SkipRule::FlagOnly, &exempt());
        assert_eq!(old.vanilla, 1, "start grant delivered the vanilla ware");
        assert_eq!(
            old.ap_reward, 0,
            "pre-fix: the AP reward is LOST (echo eaten by ECHO-DEDUP without a sale)"
        );
        let new = replay(
            &timeline,
            START_GRANT_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(
            new.vanilla, 1,
            "start grant still delivers the vanilla ware"
        );
        assert_eq!(
            new.ap_reward, 1,
            "post-fix: the check is exempt, the echo grants the AP reward exactly once"
        );
    }

    #[test]
    fn pool_receive_setting_the_acquire_flag_is_the_same_bug_shape() {
        // No uniqueStartGrants in play: the vanilla item arrives from the POOL and keyitems.rs
        // sets its obtained-flag -- the echo of this check's reward must still grant.
        let timeline = [Ev::ShopSellRun, Ev::KeyItemReceiveSetsFlag, Ev::EchoArrive];
        let old = replay(&timeline, START_GRANT_FLAG, Policy::ArmAll, SkipRule::FlagOnly, &exempt());
        assert_eq!(old.ap_reward, 0, "pre-fix loses it here too");
        let new = replay(
            &timeline,
            START_GRANT_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(new.ap_reward, 1);
    }

    #[test]
    fn ordinary_shop_check_dedup_is_unchanged() {
        // A normal shop check (stock flag NOT client-settable): the rewritten row delivers the
        // reward at purchase and the echo is skipped -- exactly one copy, before and after.
        let timeline = [Ev::ShopSellRun, Ev::Purchase, Ev::EchoArrive];
        for (policy, rule) in [
            (Policy::ArmAll, SkipRule::FlagOnly),
            (Policy::ExemptClientSettable, SkipRule::FlagAndLiveRow),
        ] {
            let g = replay(&timeline, ORDINARY_FLAG, policy, rule, &exempt());
            assert_eq!(
                g.ap_reward, 1,
                "native sale + skipped echo = exactly one reward (no double, no loss)"
            );
            assert_eq!(g.vanilla, 0);
        }
    }

    #[test]
    fn exempt_row_purchase_cannot_double_grant() {
        // The safety half of the pairing: an EXEMPT check's row is NOT rewritten, so a genuine
        // purchase sells the vanilla ware and the echo delivers the reward -- one of each, never
        // two rewards. (Arming-without-rewriting loses the item; rewriting-without-arming would
        // yield ap_reward == 2 here.)
        let g = replay(
            &[Ev::ShopSellRun, Ev::Purchase, Ev::EchoArrive],
            START_GRANT_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(g.ap_reward, 1, "echo grants the reward exactly once");
        assert_eq!(g.vanilla, 1, "the vanilla ware was what the row sold");
    }

    #[test]
    fn completed_checks_are_never_armed_regardless_of_order() {
        // run() after the start grant (the other tick order): flag already set -> shop_sell.rs
        // line 240 skips the arm even pre-fix; the echo grants. The bug was order-DEPENDENT;
        // the fix makes both orders deliver.
        let timeline = [Ev::UniqueStartGrant, Ev::ShopSellRun, Ev::EchoArrive];
        for (policy, rule) in [
            (Policy::ArmAll, SkipRule::FlagOnly),
            (Policy::ExemptClientSettable, SkipRule::FlagAndLiveRow),
        ] {
            let g = replay(&timeline, START_GRANT_FLAG, policy, rule, &exempt());
            assert_eq!(g.ap_reward, 1);
        }
    }

    #[test]
    fn param_revert_purchase_loses_the_item_pre_fix_and_delivers_it_post_fix() {
        // THE 15:45 KALÉ BUG (2026-07-24 session 2): rewrite lands at 15:37:04, a map load
        // reverts ShopLineupParam (ECHO_SKIP + flags survive), the player then buys the check
        // ware -- the row sells the VANILLA Note (screenshot: "No. Held 1"), the flag sets, and
        // the FlagOnly rule eats the echo of the real reward. Rule fix: the live row no longer
        // sells the reward, so the echo grants.
        let timeline = [Ev::ShopSellRun, Ev::ParamRevert, Ev::Purchase, Ev::EchoArrive];
        let old = replay(
            &timeline,
            ORDINARY_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagOnly,
            &exempt(),
        );
        assert_eq!(old.vanilla, 1, "reverted row sold the vanilla ware");
        assert_eq!(
            old.ap_reward, 0,
            "pre-fix: flag-only echo-skip LOSES the AP item after a param revert"
        );
        let new = replay(
            &timeline,
            ORDINARY_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(new.vanilla, 1, "the vanilla sale still happened");
        assert_eq!(
            new.ap_reward, 1,
            "post-fix: reverted row -> echo grants the AP reward exactly once"
        );
    }

    #[test]
    fn rearm_after_revert_restores_native_dedup() {
        // The other half of the fix (core.rs in-world-edge reset): revert -> re-arm -> the row
        // sells the reward again, the purchase delivers natively, and the echo is skipped --
        // exactly one reward, no vanilla leak, under the live-row rule.
        let timeline = [
            Ev::ShopSellRun,
            Ev::ParamRevert,
            Ev::Rearm,
            Ev::Purchase,
            Ev::EchoArrive,
        ];
        let g = replay(
            &timeline,
            ORDINARY_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(g.ap_reward, 1, "native sale delivered; echo deduped");
        assert_eq!(g.vanilla, 0, "no vanilla leak after the re-arm");
    }

    #[test]
    fn revert_between_purchase_and_echo_errs_toward_duplicate_never_loss() {
        // The accepted trade: buy (native reward), then a load reverts the row BEFORE the echo
        // lands. The live-row rule can no longer prove delivery, so it grants -- a duplicate.
        // Deliberate bias: a dupe is junk, a loss past the watermark is unrecoverable.
        let timeline = [Ev::ShopSellRun, Ev::Purchase, Ev::ParamRevert, Ev::EchoArrive];
        let g = replay(
            &timeline,
            ORDINARY_FLAG,
            Policy::ExemptClientSettable,
            SkipRule::FlagAndLiveRow,
            &exempt(),
        );
        assert_eq!(
            g.ap_reward, 2,
            "unprovable delivery grants: duplicate accepted, loss never"
        );
    }

    #[test]
    fn skip_decision_truth_table() {
        assert!(echo_skip_decision(true, true), "flag + live row -> skip");
        assert!(
            !echo_skip_decision(true, false),
            "flag set but row reverted/unreadable -> GRANT"
        );
        assert!(!echo_skip_decision(false, true), "flag unset -> grant");
        assert!(!echo_skip_decision(false, false));
    }

    #[test]
    fn predicate_truth_table() {
        let ex = exempt();
        assert!(
            !echo_dedup_eligible(60130, &ex),
            "client-settable -> exempt"
        );
        assert!(
            echo_dedup_eligible(100400, &ex),
            "ordinary stock flag -> eligible"
        );
        assert!(
            echo_dedup_eligible(60130, &HashSet::new()),
            "empty set -> everything eligible"
        );
    }
}
