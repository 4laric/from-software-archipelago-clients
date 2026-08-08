//! `esd_probe` -- PHASE 1 of shop auto-hints (er-archipelago#455). LOG-ONLY. No hints, no writes,
//! no behaviour change. Gated on `ER_ESD_PROBE` **or** `"probes": {"esd": true}` in
//! `apconfig.json`; with neither, the detour is never installed.
//!
//! ## What it is for
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
//! ## 🛑 Why it is behind an env var
//!
//! `invoke` is a HOT dispatch -- every line of NPC dialogue, every talk-list rebuild, many times a
//! second while a conversation is open. The standing rule is that a new hot-path hook does not ride
//! a release window (see the FMG VirtualAlloc CTD and the generalized CTD guards). An env gate
//! settles that without a version-gated feature flag: for every player who does not set it, the
//! detour is not installed and the cost is one `var_os` read at startup. Alaric sets it, runs one
//! session, and reads the log.
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
//! No argument is interpreted. Phase 2's `plan_shop_hints` is a separate change gated on what this
//! reports; guessing the range semantics here would bake an unverified premise into the artifact
//! that exists to verify it. Volume policy (watched commands always, everything else once per
//! `(talk_id, event_id)`) lives in `er_logic::esd_probe` and is unit-tested there.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the probe was asked for. Read once, at install time.
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
    if !enabled() {
        return;
    }
    let Some(target) = resolve_invoke() else {
        log::warn!(
            "ESD probe INACTIVE: the eldenring crate's RVA table rejected this executable \
             (unsupported version/language, or upstream moved the bundle) -- no shop-open \
             observation will be made this session"
        );
        return;
    };
    // SAFETY: `target` came from the crate's own vtable for this build; `esd_invoke_detour`
    // matches `EsdInvokeFn`'s calling convention exactly.
    let hook = match unsafe { GenericDetour::<EsdInvokeFn>::new(target, esd_invoke_detour) } {
        Ok(h) => h,
        Err(e) => {
            log::warn!("ESD probe INACTIVE: retour error: {e}");
            return;
        }
    };
    // SAFETY: patching a verified, executable entry inside the loaded image.
    if let Err(e) = unsafe { hook.enable() } {
        log::warn!("ESD probe INACTIVE: enable failed: {e}");
        return;
    }
    let _ = HOOK.set(hook);
    log::info!(
        "ESD probe ACTIVE (ER_ESD_PROBE set) -- logging every ESD talk command once per \
         (talk_id, event_id), and EVERY shop open (id {} buy / {} sell) with its arguments. \
         Open a merchant and grep for 'esd:'.",
        er_logic::esd_probe::OPEN_REGULAR_SHOP,
        er_logic::esd_probe::OPEN_SELL_SHOP,
    );
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
        // SAFETY: non-null checked; both pointers are the game's own live objects for this call,
        // read-only, and not retained past this frame.
        let (talk_id, event_ref) = unsafe {
            let talk_id = (*(this as *const CSEzStateTalkEvent)).talk_id;
            (talk_id, &*event)
        };
        let event_id = event_ref.id();
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
