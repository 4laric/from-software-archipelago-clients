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
//!   * Backtrace = raw frame walk (`backtrace::trace_unsynchronized`; NO dbghelp symbolization —
//!     dbghelp is not safe from an exception handler), each frame resolved to module+offset via
//!     `GetModuleHandleExW(FROM_ADDRESS)`. A stack-overflow report skips the walk (no stack to
//!     walk it on) and keeps its path minimal.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
    SetUnhandledExceptionFilter,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
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
    if let Some(code) = code && is_fatal_class(code) && take_first_chance_budget() {
        report(info as usize, "first-chance — a handler may still absorb it");
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
            if let Some(ctx) = i.ContextRecord.as_ref() {
                rip = ctx.Rip;
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
    out.push_str(&format!("thread {:?}\n", std::thread::current().id()));

    // STACK_OVERFLOW runs on the exhausted stack: skip the walk, keep the path minimal.
    if code != 0xC000_00FD {
        out.push_str("backtrace (module+offset):\n");
        let mut n = 0usize;
        // SAFETY: unsynchronized raw frame walk on the current thread only — no dbghelp, no
        // global lock to deadlock on. Bounded to 32 frames.
        unsafe {
            backtrace::trace_unsynchronized(|frame| {
                out.push_str(&format!("  {n:2}: {}\n", format_addr(frame.ip() as usize)));
                n += 1;
                n < 32
            });
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

/// `module+offset` for an address (falls back to the bare address when no module claims it —
/// JIT/trampoline/heap, e.g. a retour detour stub).
fn format_addr(addr: usize) -> String {
    match module_offset(addr) {
        Some((module, off)) => format!("{module}+{off:#x} ({addr:#x})"),
        None => format!("{addr:#x} (no module)"),
    }
}

/// Resolve an address to (module file name, offset). Same FROM_ADDRESS shape as
/// `utils::current_module_directory`, but with a fixed MAX_PATH buffer — no allocation growth
/// loops inside a crash handler.
fn module_offset(addr: usize) -> Option<(String, usize)> {
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
    Some((name, addr - base))
}
