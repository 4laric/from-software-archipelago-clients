//! The client's own output tee (clients#425).
//!
//! Every diagnostic line this client prints goes through [`emit`], which
//! duplicates it onto the real console *and*, once [`install_log_file`] has run,
//! into a durable session log.
//!
//! Why the client owns the split rather than the launcher: bb-archipelago#171
//! captured the client's output by redirecting the child's handles into
//! `<session>/client.log`, which blanked the console window players watch to see
//! what arrived; bb-archipelago#179 bought the console back with a pipe and a
//! pump thread in the launcher. Both put the tee in the parent process, so the
//! client never has a real console, and a client started by hand writes no log
//! at all. Teeing here fixes both: the console is inherited untouched, and the
//! file is written whenever `--log-file` names one, launcher or not.
//!
//! The file half is deliberately dumb -- open for append, one header, one
//! `write_all` + `flush` per line. A crash must not lose the tail, which is the
//! whole reason the log exists, so nothing is buffered across lines.

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The exact header bb-archipelago's `read_session_log_tail` slices on. It
/// finds the LAST occurrence of this prefix and reports everything after that
/// line, which is how one session's output is separated from the appended
/// history of every previous one. Changing this string silently breaks the
/// launcher's early-exit dialog, so it is pinned by test.
pub const SESSION_HEADER_PREFIX: &str = "=== SESSION START";

/// Where a line goes once the tee has split it.
#[derive(Default)]
struct Sink {
    /// The session log, present only after a successful [`install_log_file`].
    file: Option<File>,
    /// Test-only stand-in for the console half. Production always writes to the
    /// process's real stderr; a test cannot capture that portably, so it swaps
    /// in a buffer and asserts on both halves of the split.
    #[cfg(test)]
    console: Option<Vec<u8>>,
}

fn sink() -> MutexGuard<'static, Sink> {
    static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
    // A poisoned sink is not worth aborting the client over: the worst case is
    // an interleaved diagnostic line, and refusing to print at all would hide
    // the very failure being diagnosed.
    SINK.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Civil date from a day count since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
///
/// Hand-rolled rather than pulled from `chrono` because the only calendar work
/// this crate does is one header stamp, and the header's shape is pinned by a
/// cross-repository contract -- a dependency would be more surface, not less.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `YYYY-MM-DD HH:MM:SS UTC` for a Unix timestamp -- the stamp shape
/// bb-archipelago writes with `strftime("%Y-%m-%d %H:%M:%S UTC")`.
fn format_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn now_unix_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs() as i64,
        // A clock set before the epoch is absurd but not worth refusing to log
        // over: stamp the negative instant and carry on.
        Err(error) => -(error.duration().as_secs() as i64),
    }
}

/// The full header line, leading newline included.
///
/// The leading newline matters: the log is appended across sessions and the
/// previous session's last line may have no trailing newline (a client killed
/// mid-write), which would otherwise glue the header onto it and hide it from
/// the launcher's `rfind`.
fn session_header(stamp: &str) -> String {
    format!("\n{SESSION_HEADER_PREFIX} {stamp} ===\n")
}

/// Start teeing into `path`, stamping this session's header.
///
/// Call this as the first thing after argument parsing and before any other
/// output, so the file carries the whole session rather than its tail.
pub fn install_log_file(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(session_header(&format_utc(now_unix_seconds())).as_bytes())?;
    file.flush()?;
    sink().file = Some(file);
    Ok(())
}

/// Write one already-formatted line to both halves of the tee.
///
/// Called by [`crate::client_eprintln`]; not usually called directly.
pub fn emit(arguments: Arguments<'_>) {
    let line = format!("{arguments}\n");
    let mut sink = sink();
    #[cfg(test)]
    {
        if let Some(console) = sink.console.as_mut() {
            let _ = console.write_all(line.as_bytes());
        } else {
            let _ = io::stderr().write_all(line.as_bytes());
        }
    }
    #[cfg(not(test))]
    {
        let _ = io::stderr().write_all(line.as_bytes());
    }
    if let Some(file) = sink.file.as_mut() {
        // Best effort, per line: a log that cannot be written must never stop
        // the client, and the line has already reached the console regardless.
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// `eprintln!` that goes through the tee (clients#425).
///
/// Every diagnostic lane in this client uses this instead of `eprintln!`. With
/// no log file installed it is `eprintln!` -- same stream, same bytes -- which
/// is what makes "without `--log-file`, behaviour is exactly today's" true by
/// construction rather than by inspection.
#[macro_export]
macro_rules! client_eprintln {
    ($($argument:tt)*) => {
        $crate::logging::emit(::std::format_args!($($argument)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sink is process-wide, so tests that install into it must not run
    /// concurrently with each other.
    fn serialized() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Arm the console capture and clear any installed file, returning the sink
    /// to its pristine state for one test.
    fn reset_sink() {
        let mut sink = sink();
        sink.file = None;
        sink.console = Some(Vec::new());
    }

    fn captured_console() -> String {
        let mut sink = sink();
        String::from_utf8(sink.console.take().unwrap_or_default()).expect("console is UTF-8")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("bb-logging-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn format_utc_matches_the_launchers_stamp_shape() {
        // The launcher writes strftime("%Y-%m-%d %H:%M:%S UTC"); these are the
        // same instants formatted by hand, epoch and leap day included.
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00 UTC");
        assert_eq!(format_utc(1_756_049_696), "2025-08-24 15:34:56 UTC");
    }

    #[test]
    fn header_is_the_prefix_the_launcher_slices_on() {
        let header = session_header("2026-08-25 12:00:00 UTC");
        assert_eq!(header, "\n=== SESSION START 2026-08-25 12:00:00 UTC ===\n");
        assert!(
            header.starts_with('\n'),
            "a truncated last line must not hide the header"
        );
    }

    #[test]
    fn log_file_carries_the_header_and_the_line_reaches_the_console_too() {
        let _guard = serialized();
        reset_sink();
        let path = scratch("tee");
        install_log_file(&path).expect("install");
        client_eprintln!("delivered {} to {}", "Saw Cleaver", "slot");
        let console = captured_console();
        sink().file = None;
        let file = std::fs::read_to_string(&path).expect("read log");
        // Both halves of the split carry the same witnessed line.
        assert_eq!(console, "delivered Saw Cleaver to slot\n");
        assert!(
            file.contains("delivered Saw Cleaver to slot\n"),
            "file: {file:?}"
        );
        let header = file
            .lines()
            .find(|line| line.starts_with(SESSION_HEADER_PREFIX))
            .unwrap_or_else(|| panic!("no session header in {file:?}"));
        assert!(header.ends_with(" UTC ==="), "header: {header:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn without_a_log_file_nothing_is_written_anywhere_but_the_console() {
        let _guard = serialized();
        reset_sink();
        let path = scratch("untouched");
        client_eprintln!("console only");
        assert_eq!(captured_console(), "console only\n");
        assert!(
            !path.exists(),
            "no log file may be created without --log-file"
        );
    }

    #[test]
    fn appending_across_two_sessions_keeps_both_headers_and_both_bodies() {
        let _guard = serialized();
        reset_sink();
        let path = scratch("append");
        install_log_file(&path).expect("first install");
        client_eprintln!("first session line");
        install_log_file(&path).expect("second install");
        client_eprintln!("second session line");
        sink().file = None;
        let _ = captured_console();
        let file = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(
            file.matches(SESSION_HEADER_PREFIX).count(),
            2,
            "two runs must stamp two headers: {file:?}"
        );
        assert!(file.contains("first session line\n"), "{file:?}");
        assert!(file.contains("second session line\n"), "{file:?}");
        // What the launcher's tail reader would show: the second session only.
        let tail = &file[file.rfind(SESSION_HEADER_PREFIX).expect("header")..];
        assert!(tail.contains("second session line"), "{tail:?}");
        assert!(!tail.contains("first session line"), "{tail:?}");
        let _ = std::fs::remove_file(&path);
    }
}
