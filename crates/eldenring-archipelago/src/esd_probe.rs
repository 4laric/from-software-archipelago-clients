//! `esd_probe` -- the ESD talk-event detour: the shop-open seam for shop auto-hints
//! (er-archipelago#455).
//!
//! ## 🛑 PHASE 1 IS OVER. THIS HOOK NOW CARRIES A FEATURE.
//!
//! It shipped as a LOG-ONLY probe gated on `ER_ESD_PROBE` / `"probes": {"esd": true}`, whose whole
//! job was to falsify one premise: does command 22 fire at a real merchant on this build with a
//! usable row range. boblerrr's 2026-08-08 session answered yes --
//! `esd: SHOP talk 600001110 cmd 22 args [Int32(101800), Int32(101897)]` -- so the detour is now
//! installed UNCONDITIONALLY and dispatches shop opens to `crate::shop_hints`.
//!
//! The gate did not go away, it changed jobs. It now controls VERBOSITY only: with the probe off,
//! a non-shop dispatch costs one integer compare and nothing else; with it on, the phase-1
//! command-id enumeration below still runs, which is what a future session needs to pin the four
//! shop opens command 22 does not cover (Ash of War, tailoring, upgrading, change of purpose).
//!
//! ## What phase 1 was for (kept: it is why the seam is trusted)
//!
//! Shop auto-hints need a shop-open seam. The pinned crate's `examples/invoke-esd` names
//! `OPEN_REGULAR_SHOP = 22` with a ShopLineupParam row range for arguments, and
//! `cs/ez_state_talk.rs` shows the single dispatch every ESD talk event goes through. That is
//! enough to design against and NOT enough to build on: nobody has watched command 22 come out of
//! our build, at a real merchant, carrying a range we can use.
//!
//! This module buys that observation. Turn it on (env var, or the config key -- a playtester
//! should use the config key), open Kale and read the log. If no
//! `id=22` appears with a sane range, the feature dies here, before any hint logic exists.
//!
//! ## 🛑 It is a HOT dispatch, and it is now always patched
//!
//! `invoke` is the single dispatch for EVERY ESD talk event -- every line of NPC dialogue, every
//! talk-list rebuild, many times a second while a conversation is open. Phase 1 kept it behind an
//! env gate precisely because the standing rule is that a new hot-path hook does not ride a release
//! window (see the FMG VirtualAlloc CTD and the generalized CTD guards). Phase 2 cannot keep that
//! gate -- an always-on feature cannot be behind a diagnostic switch -- so the cost is paid down in
//! the detour body instead: the common path is a null check, an id read, one comparison, and the
//! trampoline. Everything expensive (the ledger, the argument formatting, the param walk) sits
//! behind either the probe flag or `event_id == 22`.
//!
//! ⚠️ That still makes this the change that must NOT be merged into an open release window on the
//! day it is written. It wants its own window and a playtest that visits merchants.
//!
//! ## 🛑 The vtable RVA is NOT reachable from here -- and that is why this looks indirect
//!
//! The obvious implementation reads `eldenring::rva::get().cs_ez_state_talk_event_vmt`. It does not
//! compile: `rva` is `pub(crate)` in the `eldenring` crate (`lib.rs`: `pub(crate) mod rva;`), so
//! the constant is shipped but not exported. The RVAs are also version- AND language-specific
//! (WW 2.6.2.0 vs JP 2.6.2.1, 0x10 apart), so hardcoding one and hoping is a CTD on the other
//! region, and hardcoding both leaves no way to tell which build we are on -- both candidates read
//! as plausible pointers.
//!
//! So we let the crate answer its own question. `TalkScript::new` is public and constructs a
//! `CSEzStateTalkEvent` whose `vftable` the crate fills in from ITS copy of the RVA table, through
//! ITS version gate. Building one throwaway `TalkScript` and reading `event.vftable.invoke` yields
//! the live function pointer with no RVA, no region check and no slot arithmetic duplicated here.
//! `TalkScript::new` is pure Rust construction (`CSTalkIns::new` is a struct literal,
//! `NpcMenuState` is `Default`) -- it registers nothing with the engine, and it drops immediately.
//!
//! ⚠️ `eldenring::rva::get()` PANICS on an unsupported executable rather than returning an error,
//! so the construction runs inside `catch_unwind`. An unsupported build therefore degrades to one
//! refusal line, which is the required behaviour anyway: if upstream moves the bundle, the feature
//! must SAY it is inactive rather than silently doing nothing.
//!
//! ## What it deliberately does not do
//!
//! It interprets exactly ONE command's arguments -- command 22's, the pair phase 1 observed. The
//! four other shop opens in the talk corpus (`OpenAshOfWarShop`, `OpenEnhanceShop`,
//! `OpenEquipmentChangeOfPurposeShop`, `OpenTailoringShop`) have NO known command id: none appeared
//! in the 08-08 log, and deriving ids by matching arguments against literal call sites is provably
//! unsound on its own (it returns `EndMachine` as the unique answer for both 119 and 120). Guessing
//! one here would put an unverified premise in a shipping path. Volume policy (watched commands
//! always, everything else once per `(talk_id, event_id)`) lives in `er_logic::esd_probe` and is
//! unit-tested there.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use eldenring::cs::{
    BlockId, CSEzStateTalkEvent, FieldInsHandle, FieldInsSelector, FieldInsType, TalkScript,
};
use eldenring::ez_state::EzStateEvent;
use er_logic::esd_probe::{EsdProbeLedger, ProbeAction};
use retour::GenericDetour;

/// The game's ESD talk-event `invoke`, as retour needs to see it.
///
/// The crate models the return as `()`. We declare `u64` on purpose: if the real function returns
/// nothing, forwarding a junk `rax` is harmless because the caller ignores it; if it DOES return
/// something the crate has not modelled, declaring `()` would drop it and corrupt the caller. Same
/// reasoning as the LuaWarp hook.
type EsdInvokeFn = unsafe extern "C" fn(*mut c_void, *const EzStateEvent) -> u64;

static HOOK: OnceLock<GenericDetour<EsdInvokeFn>> = OnceLock::new();

/// One-shot attempt latch. Every failure mode here is permanent for the running build, so a
/// per-tick retry would only spam the refusal line.
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

/// The volume ledger. `None` until first use; a poisoned lock degrades to silence, never a panic
/// inside the game's call frame.
static LEDGER: Mutex<Option<EsdProbeLedger>> = Mutex::new(None);

/// Whether the phase-1 command-id enumeration is on. Read once at install, then on every dispatch
/// -- so it must stay a plain relaxed atomic load, not a config or env read.
static PROBE_VERBOSE: AtomicBool = AtomicBool::new(false);

/// NPC talk ESD can temporarily rebuild the key-item inventory while `in_world` and the player
/// pointer both remain valid. The reconciler must not diff or grant against that transient view.
static TALK_CLOCK: OnceLock<Instant> = OnceLock::new();
static LAST_TALK_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

/// ESD invokes many times a second while a conversation is active, so the window slides until the
/// script is actually quiet rather than assuming a particular bell-bearing command id.
const INVENTORY_QUIET_MS: u64 = 2_000;

fn talk_clock_ms() -> u64 {
    TALK_CLOCK
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
        + 1 // zero is the "no talk observed" sentinel
}

/// Whether received-item inventory work is safe this frame. Flags deliberately do not use this:
/// they have no inventory pointer and remain self-healing while a conversation is open.
pub fn inventory_grants_safe() -> bool {
    er_logic::esd_probe::inventory_quiet(
        talk_clock_ms(),
        LAST_TALK_ACTIVITY_MS.load(Ordering::Relaxed),
        INVENTORY_QUIET_MS,
    )
}

/// Whether the phase-1 ENUMERATION was asked for. Read once, at install time. It no longer gates
/// the detour itself -- shop auto-hints need the hook whatever this says.
fn enabled() -> bool {
    shared::probes::enabled("ER_ESD_PROBE", "esd")
}

/// Ask the `eldenring` crate for the live `CSEzStateTalkEvent::invoke`, via a throwaway
/// `TalkScript`. See the module docs for why this is not an RVA read.
///
/// Returns `None` if the crate's own version gate rejects this executable (it panics; we catch).
fn resolve_invoke() -> Option<EsdInvokeFn> {
    let built = std::panic::catch_unwind(|| {
        // A syntactically valid, semantically meaningless handle: nothing dereferences it, because
        // nothing ever runs this talkscript. `BlockId::none()` is the crate's own "not tied to a
        // map" value.
        let handle = FieldInsHandle {
            selector: FieldInsSelector::from_parts(FieldInsType::Chr, 0, 0),
            block_id: BlockId::none(),
        };
        let script = TalkScript::new(BlockId::none(), 0, handle);
        // Copy the pointer OUT before the script drops. `vftable` derefs to the generated vtable
        // layout, whose `invoke` field is the game's function.
        let invoke = script.event.vftable.invoke as usize;
        // SAFETY: rebuilding a bare `extern "C"` fn pointer from the address the crate's own
        // vtable holds. Same ABI, same address; only the argument types are erased and the
        // return made explicit (see `EsdInvokeFn`).
        unsafe { std::mem::transmute::<usize, EsdInvokeFn>(invoke) }
    });
    built.ok()
}

/// Install the probe. Call from `core::update_live` (game thread). Self-guarded one-shot; every
/// failure degrades to a log line, so the caller needs no latch.
pub fn install() {
    if ATTEMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    PROBE_VERBOSE.store(enabled(), Ordering::Relaxed);
    let Some(target) = resolve_invoke() else {
        log::warn!(
            "SHOP HINTS INACTIVE: the eldenring crate's RVA table rejected this executable \
             (unsupported version/language, or upstream moved the bundle), so the ESD talk hook \
             could not be resolved -- opening a merchant will announce nothing this session"
        );
        crate::shop_hints::mark_inactive();
        return;
    };
    // SAFETY: `target` came from the crate's own vtable for this build; `esd_invoke_detour`
    // matches `EsdInvokeFn`'s calling convention exactly.
    let hook = match unsafe { GenericDetour::<EsdInvokeFn>::new(target, esd_invoke_detour) } {
        Ok(h) => h,
        Err(e) => {
            log::warn!("SHOP HINTS INACTIVE: retour could not build the ESD talk detour: {e}");
            crate::shop_hints::mark_inactive();
            return;
        }
    };
    // SAFETY: patching a verified, executable entry inside the loaded image.
    if let Err(e) = unsafe { hook.enable() } {
        log::warn!("SHOP HINTS INACTIVE: enabling the ESD talk detour failed: {e}");
        crate::shop_hints::mark_inactive();
        return;
    }
    let _ = HOOK.set(hook);
    log::info!(
        "ESD talk hook ACTIVE -- shop auto-hints armed on command {} (the regular buy menu). \
         Grep for 'shop-hints:'.",
        er_logic::esd_probe::OPEN_REGULAR_SHOP,
    );
    if PROBE_VERBOSE.load(Ordering::Relaxed) {
        let gate = shared::probes::source("ER_ESD_PROBE", "esd")
            .unwrap_or_else(|| "unknown gate".to_string());
        log::info!(
            "ESD command enumeration ACTIVE (via {gate}) -- logging every ESD talk command once \
             per (talk_id, event_id), and EVERY shop open (id {} buy / {} sell) with its \
             arguments. Grep for 'esd:'.",
            er_logic::esd_probe::OPEN_REGULAR_SHOP,
            er_logic::esd_probe::OPEN_SELL_SHOP,
        );
    }
}

/// Render up to `MAX_LOGGED_ARGS` arguments. Only called when we have already decided to log, so
/// the string work never lands on a suppressed dispatch.
const MAX_LOGGED_ARGS: usize = 12;

fn render_args(event: &EzStateEvent) -> String {
    let len = event.args.len();
    let shown = len.min(MAX_LOGGED_ARGS);
    let mut out = String::new();
    for i in 0..shown {
        // 🛑 Bound on `len`, NOT on `arg()`'s own answer: the crate's `arg` tests `index > len`,
        // so `index == len` returns `Some` of an out-of-bounds slot. Never ask it for that index.
        if let Some(v) = event.arg(i as u32) {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{v:?}"));
        }
    }
    if len > shown {
        out.push_str(&format!(", ... ({} more)", len - shown));
    }
    out
}

/// The detour body. Runs INSIDE the game's ESD dispatch, on the game thread, for every talk event.
/// Must never panic across the FFI boundary and must never change what the game does.
unsafe extern "C" fn esd_invoke_detour(this: *mut c_void, event: *const EzStateEvent) -> u64 {
    // Everything observational is caught. A poisoned mutex, a formatting panic, a surprise in the
    // event layout -- none of them may take the game down, and none of them may skip the
    // trampoline below.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.is_null() || event.is_null() {
            return;
        }
        // Record EVERY valid talk dispatch before interpreting it. This is the inventory-stability
        // seam; it covers the Twin Maiden hand-in commands as well as the shop-open command whose
        // id we understand.
        LAST_TALK_ACTIVITY_MS.store(talk_clock_ms(), Ordering::Relaxed);
        // SAFETY: non-null checked; both pointers are the game's own live objects for this call,
        // read-only, and not retained past this frame.
        let (talk_id, event_ref) = unsafe {
            let talk_id = (*(this as *const CSEzStateTalkEvent)).talk_id;
            (talk_id, &*event)
        };
        let event_id = event_ref.id();

        // ---- PHASE 2: THE FEATURE ----------------------------------------------------------
        // Runs whether or not the enumeration is on -- the hint is the point; the probe flag only
        // decides how loud the log is. This is also the ONLY work a non-shop dispatch can reach
        // when the probe is off, which is what keeps an always-installed hot-path hook cheap.
        if event_id == er_logic::esd_probe::OPEN_REGULAR_SHOP {
            // 🛑 Bound on `args.len()`, never on `arg()`'s own answer: the crate's `arg` tests
            // `index > len`, so `index == len` hands back `Some` of an out-of-bounds slot.
            if event_ref.args.len() < 2 {
                log::warn!(
                    "shop-hints: command {} fired with {} argument(s), expected a row range \
                     -- nothing hinted for this open",
                    er_logic::esd_probe::OPEN_REGULAR_SHOP,
                    event_ref.args.len()
                );
            } else if let (Some(lo), Some(hi)) = (event_ref.arg(0), event_ref.arg(1)) {
                let (lo, hi) = (i32::from(lo), i32::from(hi));
                crate::shop_hints::on_shop_open(lo, hi);
                // er-archipelago#325. Same range, same frame, and deliberately AFTER the hint: the
                // hint describes the shelf as the player is about to see it, and handing the bell
                // in changes nothing about THIS shelf (it adds a menu entry at the Twin Maidens),
                // so the order is not load-bearing -- but a feature added later goes second.
                crate::merchant_bells::on_shop_open(lo, hi);
                // #937 hover probe: start the goods-lookup cursor trace. Cheap gate inside (the
                // probe's ACTIVE atomic); placed after the features for the same reason they are
                // ordered -- a diagnostic added later goes last.
                crate::hover_probe::on_shop_open(lo, hi);
            }
        }

        if !PROBE_VERBOSE.load(Ordering::Relaxed) {
            return;
        }

        // ---- PHASE 1: THE COMMAND-ID ENUMERATION (diagnostic only) -------------------------
        let action = {
            let Ok(mut guard) = LEDGER.lock() else {
                return;
            };
            guard
                .get_or_insert_with(EsdProbeLedger::new)
                .observe(talk_id, event_id)
        };
        match action {
            ProbeAction::Skip => {}
            ProbeAction::LogWatched => {
                let args = render_args(event_ref);
                log::info!("esd: SHOP talk {talk_id} cmd {event_id} args [{args}]");
            }
            ProbeAction::LogFirstSighting => {
                let args = render_args(event_ref);
                log::info!("esd: talk {talk_id} cmd {event_id} args [{args}] (first sighting)");
            }
        }
    }));
    // Always call through, whatever happened above. The None arm is unreachable in practice --
    // install() runs on the game thread, the same thread that dispatches -- but a swallowed ESD
    // event would freeze a conversation, so it must be loud rather than silent.
    match HOOK.get() {
        // SAFETY: trampoline to the original, same args, same convention.
        Some(h) => unsafe { h.call(this, event) },
        None => {
            log::error!("esd: trampoline missing (enable/set race?) -- talk event swallowed");
            0
        }
    }
}
