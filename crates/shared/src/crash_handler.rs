//! Native crash telemetry — name the faulting site before the process dies.
//!
//! MOTIVATION (2026-07-24 CTD, warp to "Beside the Rampart Gaol"): the game died mid fast-travel
//! and the log simply ENDED at the warp line — no faulting module, no offset, nothing to tell an
//! engine-side crash from any of the client's own engine-memory touchers (flag poll, ChrIns
//! sweep, FieldArea gate write). `handle_panics` only covers Rust panics; a native access
//! violation dies silently. This module makes the NEXT such crash name its site as
//! `module+offset` — the same RVA space the codebase already pins (warp.rs, detour.rs).
//!
//! Two registrations, one report path:
//!   * `SetUnhandledExceptionFilter` — the definitive "this exception is killing the process"
//!     record (runs only after no SEH frame claimed the exception). Chains to the previously
//!     installed filter (WER / Steam / the game's own) — we observe, never swallow.
//!   * `AddVectoredExceptionHandler(first = 1)` — a FIRST-CHANCE record for fatal-class codes
//!     (AV, illegal instruction, stack overflow, ...), because a crash inside someone else's
//!     handler chain — or a `TerminateProcess` from a crash reporter — can keep the top-level
//!     filter from ever running. First-chance records are CAPPED (software that throws and
//!     handles AVs on purpose must not flood the log) and labeled `first-chance`: the process
//!     MAY survive one; the record that matters is the last one before the log ends.
//!
//! DISCIPLINE — never make a crash worse:
//!   * Never panic, never unwind across the OS dispatch boundary: reentrancy latch + the whole
//!     report body under `catch_unwind`, every OS call best-effort.
//!   * The report is appended to a dedicated `crash-<pid>.txt` beside the log FIRST, through a
//!     fresh file handle (no lock shared with anything), and only THEN mirrored via
//!     `log::error!` — if the crashing thread was inside the logger when it faulted, the direct
//!     file still lands even if the mirror deadlocks a process that is dying anyway.
//!   * Backtrace = manual unwind of the FAULTING thread's `CONTEXT` (`RtlLookupFunctionEntry` +
//!     `RtlVirtualUnwind` on a local copy; NO dbghelp symbolization — dbghelp is not safe from
//!     an exception handler). Walking our OWN stack here names the handler, not the crash: the
//!     first live report (2026-07-24) spent frames 0-2 on this module's report machinery and
//!     never showed the game frames that led to the fault. Each frame resolves to module+offset
//!     via `GetModuleHandleExW(FROM_ADDRESS)`, and the module bases seen are listed so an
//!     offset (RVA) stays comparable across ASLR'd sessions. A stack-overflow report skips the
//!     walk (the handler itself runs on whatever stack is left) and keeps its path minimal.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
    RtlLookupFunctionEntry, RtlVirtualUnwind, SetUnhandledExceptionFilter, UNW_FLAG_NHANDLER,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
    VirtualQuery,
};
use windows::core::PCWSTR;

/// Directory the `crash-<pid>.txt` report is written into (the log dir). Set once by [install].
static REPORT_DIR: OnceLock<PathBuf> = OnceLock::new();
/// The top-level filter that was installed before ours; chained after our report so the previous
/// behavior (WER dialog, Steam reporter, ...) is preserved exactly.
static PREV_FILTER: OnceLock<PrevFilter> = OnceLock::new();
/// One-shot install latch.
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Reentrancy latch: a fault inside our own report falls straight through to the OS.
static IN_HANDLER: AtomicBool = AtomicBool::new(false);
/// First-chance report budget. The top-level (process-dying) report is never budgeted.
static FIRST_CHANCE_LEFT: AtomicUsize = AtomicUsize::new(5);

/// `LPTOP_LEVEL_EXCEPTION_FILTER` is an `Option<fn>` — wrap it so it can live in a `OnceLock`
/// (raw fn pointers into foreign code are neither `Send` nor `Sync` by inference).
struct PrevFilter(LPTOP_LEVEL_EXCEPTION_FILTER);
// SAFETY: the value is written once at install and only ever read to call a process-global
// callback the OS itself invokes from arbitrary threads.
unsafe impl Send for PrevFilter {}
unsafe impl Sync for PrevFilter {}

/// SEH dispatcher verdict: keep searching handlers (we only observe, never claim).
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

/// Install both handlers (idempotent). `report_dir` should be the log directory so the crash
/// file lands next to the session log users already send in. Best-effort: a registration
/// failure degrades to a warning, never an error.
pub fn install(report_dir: PathBuf) {
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = REPORT_DIR.set(report_dir);
    // SAFETY: registering process-wide callbacks with the exact signatures the OS expects; both
    // registrations are documented safe at any time and load no library (loader-lock-safe —
    // install runs from DllMain via start_logger).
    unsafe {
        let prev = SetUnhandledExceptionFilter(Some(top_level_filter));
        let _ = PREV_FILTER.set(PrevFilter(prev));
        if AddVectoredExceptionHandler(1, Some(first_chance_handler)).is_null() {
            log::warn!(
                "crash-handler: AddVectoredExceptionHandler failed — first-chance telemetry off (top-level filter still armed)"
            );
        }
    }
    log::info!(
        "crash-handler armed: fatal exceptions will be reported as module+offset (crash-<pid>.txt beside the log, mirrored into this log)"
    );
}

/// The unhandled filter — the process is dying. Report, then chain to the previous filter.
unsafe extern "system" fn top_level_filter(info: *const EXCEPTION_POINTERS) -> i32 {
    report(info as usize, "UNHANDLED — process dying");
    match PREV_FILTER.get() {
        // SAFETY: chaining to the previously registered filter with the same pointer the OS
        // handed us — exactly the call the OS would have made had we never installed.
        Some(PrevFilter(Some(prev))) => unsafe { prev(info) },
        _ => EXCEPTION_CONTINUE_SEARCH,
    }
}

/// Claim one unit of the first-chance report budget; `false` once it is spent.
///
/// A hand-rolled CAS loop rather than `fetch_update`: that method is deprecated (renamed
/// `try_update`) on current stable, and CI builds with `-D warnings`, so calling it FAILS THE
/// BUILD — which is exactly what happened to `dc2bd41`, leaving the crash handler we shipped to
/// diagnose the CTD unbuildable. `try_update` would fix it only for toolchains new enough to have
/// it; `compare_exchange_weak` has been stable for years and cannot rot the same way. The
/// semantics are unchanged: saturating decrement, and we report only if WE were the one who
/// decremented (so the budget is never over-drawn under concurrent faults).
fn take_first_chance_budget() -> bool {
    let mut left = FIRST_CHANCE_LEFT.load(Ordering::Relaxed);
    while left > 0 {
        match FIRST_CHANCE_LEFT.compare_exchange_weak(
            left,
            left - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => left = observed,
        }
    }
    false
}

/// The vectored handler — first-chance. Reports fatal-class codes only, under a session budget,
/// and ALWAYS continues the search (zero effect on dispatch).
unsafe extern "system" fn first_chance_handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the OS hands a valid EXCEPTION_POINTERS for the duration of the call; read-only.
    let code = unsafe {
        info.as_ref()
            .and_then(|i| i.ExceptionRecord.as_ref())
            .map(|r| r.ExceptionCode.0 as u32)
    };
    if let Some(code) = code
        && is_fatal_class(code)
        && take_first_chance_budget()
    {
        report(
            info as usize,
            "first-chance — a handler may still absorb it",
        );
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Fatal-class exception codes worth a first-chance record. Everything else — C++ exceptions
/// (0xE06D7363), breakpoints, guard pages, the 0x406D1388 thread-name convention — is normal
/// control flow somewhere and none of our business.
fn is_fatal_class(code: u32) -> bool {
    matches!(
        code,
        0xC000_0005 // ACCESS_VIOLATION
            | 0xC000_0006 // IN_PAGE_ERROR
            | 0xC000_001D // ILLEGAL_INSTRUCTION
            | 0xC000_0025 // NONCONTINUABLE_EXCEPTION
            | 0xC000_008C // ARRAY_BOUNDS_EXCEEDED
            | 0xC000_0094 // INT_DIVIDE_BY_ZERO
            | 0xC000_0096 // PRIV_INSTRUCTION
            | 0xC000_00FD // STACK_OVERFLOW
            | 0xC000_0374 // HEAP_CORRUPTION
    )
}

fn code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "ACCESS_VIOLATION",
        0xC000_0006 => "IN_PAGE_ERROR",
        0xC000_001D => "ILLEGAL_INSTRUCTION",
        0xC000_0025 => "NONCONTINUABLE_EXCEPTION",
        0xC000_008C => "ARRAY_BOUNDS_EXCEEDED",
        0xC000_0094 => "INT_DIVIDE_BY_ZERO",
        0xC000_0096 => "PRIV_INSTRUCTION",
        0xC000_00FD => "STACK_OVERFLOW",
        0xC000_0374 => "HEAP_CORRUPTION",
        _ => "?",
    }
}

/// Reentrancy-latched, unwind-proof report wrapper. `info` travels as `usize` so the closure is
/// trivially unwind-safe.
fn report(info: usize, phase: &str) {
    if IN_HANDLER.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::panic::catch_unwind(|| report_inner(info, phase));
    IN_HANDLER.store(false, Ordering::SeqCst);
}

fn report_inner(info: usize, phase: &str) {
    let info = info as *const EXCEPTION_POINTERS;
    let mut code = 0u32;
    let mut fault_addr = 0usize;
    let mut av_detail = String::new();
    let mut rip = 0u64;
    let mut rsp = 0u64;
    let mut ctx_ptr: *const CONTEXT = std::ptr::null();
    // SAFETY: pointers come straight from the OS dispatcher and are valid for the call; every
    // dereference is null-checked. Read-only.
    unsafe {
        if let Some(i) = info.as_ref() {
            if let Some(rec) = i.ExceptionRecord.as_ref() {
                code = rec.ExceptionCode.0 as u32;
                fault_addr = rec.ExceptionAddress as usize;
                // AV / in-page carry [op, target]: op 0 = read, 1 = write, 8 = DEP-execute.
                if matches!(code, 0xC000_0005 | 0xC000_0006) && rec.NumberParameters >= 2 {
                    let op = match rec.ExceptionInformation[0] {
                        0 => "read",
                        1 => "write",
                        8 => "execute",
                        _ => "access",
                    };
                    av_detail = format!(" ({op} at {:#x})", rec.ExceptionInformation[1]);
                }
            }
            ctx_ptr = i.ContextRecord.cast_const();
            if let Some(ctx) = ctx_ptr.as_ref() {
                rip = ctx.Rip;
                rsp = ctx.Rsp;
            }
        }
    }

    let mut out = String::with_capacity(1024);
    out.push_str(&format!(
        "=== NATIVE CRASH [{phase}] ===\nexception {code:#010x} {}{av_detail}\n",
        code_name(code)
    ));
    out.push_str(&format!("at  {}\n", format_addr(fault_addr)));
    if rip != 0 && rip as usize != fault_addr {
        out.push_str(&format!("rip {}\n", format_addr(rip as usize)));
    }
    if rsp != 0 {
        out.push_str(&format!("rsp {rsp:#x}\n"));
    }
    out.push_str(&format!("thread {:?}\n", std::thread::current().id()));

    // STACK_OVERFLOW runs on the exhausted stack: skip the walk, keep the path minimal.
    if code != 0xC000_00FD {
        let mut frames = [0usize; MAX_FRAMES];
        // SAFETY: the context pointer came from the OS dispatcher and stays valid for the whole
        // handler call; walk_fault_context copies it before unwinding anything.
        let mut nframes = match unsafe { ctx_ptr.as_ref() } {
            Some(ctx) => walk_fault_context(ctx, &mut frames),
            None => 0,
        };
        if nframes > 0 {
            out.push_str("backtrace (module+offset):\n");
        } else {
            // No context to walk (or its rip was garbage from the start): degrade to the old
            // self-stack walk rather than report nothing — and say so in the header, because
            // these frames name the handler, not the fault.
            nframes = walk_own_stack(&mut frames);
            out.push_str("backtrace (module+offset, HANDLER stack — fault context unusable):\n");
        }
        for (i, &addr) in frames[..nframes].iter().enumerate() {
            out.push_str(&format!("  {i:2}: {}\n", format_addr(addr)));
        }
        // Module bases behind the module+offset lines above: ASLR moves the base every launch;
        // the offset (RVA) is the part that stays comparable across sessions — record both
        // sides of that equation.
        let mut mods: Vec<(String, usize)> = Vec::with_capacity(4);
        for &addr in frames[..nframes].iter().chain(std::iter::once(&fault_addr)) {
            if let Some((name, base, _)) = module_offset(addr)
                && !mods.iter().any(|(_, b)| *b == base)
            {
                mods.push((name, base));
            }
        }
        if !mods.is_empty() {
            out.push_str("modules:\n");
            for (name, base) in &mods {
                out.push_str(&format!("  {name} @ {base:#x}\n"));
            }
        }
    }

    // Direct file FIRST (own handle, no shared locks), then the log mirror + flush.
    let dir = REPORT_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let path = dir.join(format!("crash-{}.txt", std::process::id()));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(
            f,
            "{} {out}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let _ = f.sync_all();
    }
    log::error!("{out}");
    log::logger().flush();
}

/// Cap on recorded frames — same bound the old self-stack walk used.
const MAX_FRAMES: usize = 32;

/// Top of the x64 user-mode address range; an rip/rsp beyond it means the frame chain is
/// corrupt, and the walk stops rather than chase garbage.
const USER_ADDR_MAX: u64 = 0x7FFF_FFFF_FFFF;

/// A `CONTEXT` copy carrying the 16-byte alignment the OS requires of one. The windows-rs
/// struct is plain `repr(C)` (8-aligned), and `RtlVirtualUnwind` restores nonvolatile XMM state
/// with aligned stores — an under-aligned copy could itself fault, inside the crash handler.
#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// Unwind the FAULTING thread's stack from the exception `CONTEXT`, recording `Rip` per frame;
/// returns the frame count. This is the same table walk the OS dispatcher performs on this very
/// context during SEH search — `RtlLookupFunctionEntry` + `RtlVirtualUnwind`, both kernel32
/// exports with no dbghelp state and no lock we could already hold — done on a local COPY so
/// the context the OS still owns is never mutated.
fn walk_fault_context(ctx: &CONTEXT, frames: &mut [usize; MAX_FRAMES]) -> usize {
    let mut ctx = AlignedContext(*ctx);
    let mut n = 0usize;
    while n < MAX_FRAMES {
        let rip = ctx.0.Rip;
        if rip == 0 || rip > USER_ADDR_MAX {
            break;
        }
        frames[n] = rip as usize;
        n += 1;
        let rsp = ctx.0.Rsp;
        if rsp == 0 || rsp > USER_ADDR_MAX || !readable(rsp as usize) {
            break;
        }
        let mut image_base = 0u64;
        // SAFETY: a read-only lookup over the loader's function tables; a null result just
        // means no unwind info exists for this rip.
        let entry = unsafe { RtlLookupFunctionEntry(rip, &raw mut image_base, None) };
        if entry.is_null() {
            if n > 1 {
                // Mid-walk rip with no unwind info: JIT/trampoline code (e.g. a retour detour
                // stub). Guessing a frame size past it would fabricate frames — stop; the
                // recorded "(no module)" address is itself the evidence of the detour.
                break;
            }
            // Faulting rip with no unwind info at frame 0: a fault in a true leaf function, or
            // a jump/call to a bad address — either way the return address sits untouched at
            // [rsp], so simulate the pop.
            // SAFETY: readable(rsp) above verified the page is committed and readable, and an
            // 8-aligned 8-byte read cannot cross into the next page.
            ctx.0.Rip = unsafe { (rsp as *const u64).read_unaligned() };
            ctx.0.Rsp = rsp + 8;
        } else {
            let mut handler_data: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut establisher_frame = 0u64;
            // SAFETY: unwinds one frame of the local aligned copy using the entry just looked
            // up for this exact rip; out-params are live locals. readable(rsp) fences off the
            // common nested-fault case (garbage rsp), though a frame extending past the queried
            // region could still in principle fault — accepted residual risk, latched by
            // IN_HANDLER either way.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image_base,
                    rip,
                    entry,
                    &raw mut ctx.0,
                    &raw mut handler_data,
                    &raw mut establisher_frame,
                    None,
                );
            }
        }
        if ctx.0.Rsp <= rsp {
            // Rsp must strictly rise as frames pop; anything else is a corrupt chain (or the
            // end of the stack) — stop.
            break;
        }
    }
    n
}

/// Fallback when the OS handed us no usable `CONTEXT`: walk OUR OWN stack. The frames name the
/// handler machinery rather than the fault site — the report header says so — but a labeled,
/// degraded backtrace still beats none.
fn walk_own_stack(frames: &mut [usize; MAX_FRAMES]) -> usize {
    let mut n = 0usize;
    // SAFETY: unsynchronized raw frame walk on the current thread only — no dbghelp, no global
    // lock to deadlock on. Bounded to MAX_FRAMES.
    unsafe {
        backtrace::trace_unsynchronized(|frame| {
            frames[n] = frame.ip() as usize;
            n += 1;
            n < MAX_FRAMES
        });
    }
    n
}

/// Best-effort "safe to read a stack slot here": committed, and neither PAGE_GUARD nor
/// PAGE_NOACCESS. Keeps our own leaf-frame read — and the common failure mode of
/// `RtlVirtualUnwind`'s stack reads — from raising a NESTED fault inside the crash handler.
fn readable(addr: usize) -> bool {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: writes only into the live local struct, bounded by that struct's true size.
    let len = unsafe {
        VirtualQuery(
            Some(addr as *const std::ffi::c_void),
            &raw mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    len != 0
        && info.State == MEM_COMMIT
        && (info.Protect & (PAGE_GUARD | PAGE_NOACCESS)) == PAGE_PROTECTION_FLAGS(0)
}

/// `module+offset` for an address (falls back to the bare address when no module claims it —
/// JIT/trampoline/heap, e.g. a retour detour stub).
fn format_addr(addr: usize) -> String {
    match module_offset(addr) {
        Some((module, _base, off)) => format!("{module}+{off:#x} ({addr:#x})"),
        None => format!("{addr:#x} (no module)"),
    }
}

/// Resolve an address to (module file name, module base, offset). Same FROM_ADDRESS shape as
/// `utils::current_module_directory`, but with a fixed MAX_PATH buffer — no allocation growth
/// loops inside a crash handler. The base rides along so the report can list it next to the
/// offsets that were computed against it.
fn module_offset(addr: usize) -> Option<(String, usize, usize)> {
    let mut module = HMODULE::default();
    // SAFETY: FROM_ADDRESS treats the "name" pointer as an address to resolve;
    // UNCHANGED_REFCOUNT takes no reference. Failure -> None.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr as *const u16),
            &raw mut module,
        )
        .ok()?;
    }
    let base = module.0 as usize;
    if base == 0 || addr < base {
        return None;
    }
    let mut buf = [0u16; 260];
    // SAFETY: writes at most buf.len() u16s into a live stack buffer.
    let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    if n == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    let name = full
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(full.as_str())
        .to_string();
    Some((name, base, addr - base))
}
