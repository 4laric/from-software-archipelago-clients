//! The client's OWN pinned RVAs, dispatched per detected executable version.
//!
//! # Why this module exists
//!
//! The `eldenring` crate carries 93 RVAs and picks a table per game version by itself. This
//! client carries **eight more** that the crate knows nothing about -- the AddItemFunc detour,
//! the inventory pointer slot, LuaWarp and its `CSLuaEventManager` slot candidates, the FMG
//! repository and its `SearchStringTable`, and `ChrAsm::operator=`. Until #241 those eight were
//! bare `const`s baked to Elden Ring 2.6.2.0, so the moment the crate learned a second Worldwide
//! version they would have gone on pointing into the old build's addresses.
//!
//! They now select off the SAME detected-version source the startup gate uses
//! ([`crate::game_version_gate::detected`]), so the crate's table and ours can never disagree
//! about which executable is running.
//!
//! # 🛑 The 2.7.0.0 column is CANDIDATE data
//!
//! Every address in [`WW270`] was derived offline from the 2.7.0.0 executable and **has never
//! been executed**. Method: for code addresses, a capstone-masked byte window matched against
//! the old function; for data addresses, locating rip-relative instructions that reference the
//! old address, matching their masked context in the new `.text`, and re-reading the
//! displacement -- a vote over up to 20 reference sites. Every delta below falls inside a band
//! that the crate's independent 93-entry port established for the same PE section, which is a
//! cross-check these constants have never had before. It is still not execution.
//!
//! The `_SIG` prologue guards at each call site are the safety net, and they fail CLOSED: a
//! stale address disables that feature rather than jumping into the middle of something else.

use crate::game_version_gate::{Supported, detected};

/// The eight client-private RVAs for one executable build.
pub struct ClientRvas {
    /// `AddItemFunc` -- the item-grant hook target (`detour`).
    pub add_item_func: usize,
    /// Static slot holding the inventory pointer (`detour`, diagnostic confirm path).
    pub inventory_ptrloc: usize,
    /// `LuaWarp` entry (`warp`).
    pub lua_warp_func: usize,
    /// `CSLuaEventManager` static-slot candidates, in probe order (`warp`).
    pub cslem_candidates: [usize; 2],
    /// FMG repository static slot (`fmg_inject`).
    pub fmg_repo: usize,
    /// `SearchStringTable` (`fmg_inject`).
    pub fmg_search: usize,
    /// `ChrAsm::operator=` -- the refcounted equip commit (`auto_equip`).
    pub chr_asm_commit: usize,
}

/// Elden Ring 2.6.2.0 Worldwide. VERIFIED: these are the addresses the client has shipped and
/// played on.
pub const WW262: ClientRvas = ClientRvas {
    add_item_func: 0x0056_05B0,
    inventory_ptrloc: 0x03D6_7A50,
    lua_warp_func: 0x0059_9C10,
    cslem_candidates: [0x03D6_7E48, 0x03D5_AFE0],
    fmg_repo: 0x03D7_D4F8,
    fmg_search: 0x0266_D3C0,
    chr_asm_commit: 0x0024_5C00,
};

/// Elden Ring 2.7.0.0 Worldwide (Tarnished Edition). 🛑 CANDIDATE -- derived 2026-08-27, never
/// executed. See the module header.
pub const WW270: ClientRvas = ClientRvas {
    // delta 0xE50, shared with lua_warp_func; unique masked 64-byte window.
    add_item_func: 0x0056_1400,
    // delta 0x4070; dataref vote 15/20.
    inventory_ptrloc: 0x03D6_BAC0,
    // delta 0xE50; unique masked 64-byte window.
    lua_warp_func: 0x0059_AA60,
    // deltas 0x4070 / 0x4060; dataref votes 16/20 and 17/20.
    cslem_candidates: [0x03D6_BEB8, 0x03D5_F040],
    // 🛑 MEDIUM CONFIDENCE, the weakest row in the whole port: only 2 of 20 sampled reference
    // sites voted, because the surrounding code changed enough that most context windows did not
    // match. The winning delta 0x4070 does agree with the other three `.data` ports, which is why
    // it is here at all. VERIFY THIS ONE FIRST -- a wrong repo pointer is read, not called, so no
    // prologue guard covers it; `plausible(repo_addr)` in `fmg_inject` is the only screen.
    fmg_repo: 0x03D8_1568,
    // delta 0x2810 (the high-.text band); unique masked hit at 16/24/32/48-byte windows.
    fmg_search: 0x0266_FBD0,
    // UNCHANGED from 2.6.2.0 -- low `.text` did not shift in this patch.
    chr_asm_commit: 0x0024_5C00,
};

/// The table for the executable we are actually running in.
///
/// Falls back to [`WW262`] when detection fails. That arm is not reachable in a normal session:
/// `DllMain` runs [`crate::game_version_gate::check`] first and refuses to initialise anything at
/// all on an executable we have no table for, so nothing that reads these RVAs gets built. The
/// fallback picks the VERIFIED column rather than the candidate one on principle.
///
/// JP 2.6.2.1 also maps here to [`WW262`]. That is not a claim these eight addresses are correct
/// on the Japanese executable -- they were never derived for it. It preserves exactly what the
/// client did before this module existed (one baked Worldwide constant for every build), and the
/// per-call-site `_SIG` prologue guards are what actually keep it honest there.
pub fn current() -> &'static ClientRvas {
    match detected() {
        Some(Supported::Ww270) => &WW270,
        Some(Supported::Ww262) | Some(Supported::Jp2621) | None => &WW262,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `chr_asm_commit` is the one address the port found UNCHANGED. If a future edit "fixes" it
    /// by applying some delta, this catches it -- an unchanged address is a finding, not an
    /// oversight.
    #[test]
    fn chr_asm_commit_is_deliberately_identical_across_builds() {
        assert_eq!(WW262.chr_asm_commit, WW270.chr_asm_commit);
    }

    /// Everything else moved. A column that silently copied 2.6.2.0 wholesale would look like a
    /// port and be a no-op, which is the failure mode worth a test.
    #[test]
    fn every_other_2700_address_actually_moved() {
        assert_ne!(WW262.add_item_func, WW270.add_item_func);
        assert_ne!(WW262.inventory_ptrloc, WW270.inventory_ptrloc);
        assert_ne!(WW262.lua_warp_func, WW270.lua_warp_func);
        assert_ne!(WW262.cslem_candidates, WW270.cslem_candidates);
        assert_ne!(WW262.fmg_repo, WW270.fmg_repo);
        assert_ne!(WW262.fmg_search, WW270.fmg_search);
    }
}
