//! Loaded-character identity from shadPS4's own save I/O log.
//!
//! Bloodborne persists character slots as `/savedata0/userdata0000` through
//! `userdata0009`; `userdata0010` is global data and must never identify a
//! character. We only accept write opens (`flags = 0x201`), since the title
//! screen reads every slot while listing characters. Starting at the current
//! end of the log makes every identity observation belong to this attachment,
//! rather than a previous emulator run left in the append-only file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

const PREFIX: &str = "open: path = /savedata0/userdata";
const WRITE_FLAGS: &str = "flags = 0x201";
/// shadPS4's host-side open failure, logged right after a save write-open
/// the emulator could not honour.
const HOST_OPEN_FAILURE: &str = "Failed to open the file at path=";

/// How many `poll()` cycles we give the save file to prove, via its own
/// mtime, that it is still being written despite a denied open, before we
/// give up corroborating and escalate to the loud warning. clients#619: on
/// at least one real machine every guest-level write-open was denied all
/// session while the save kept landing through a path the fs log never
/// shows, so the denial alone is not evidence of data loss.
const DENIAL_SETTLE_POLLS: u32 = 3;

/// The outcome of a save write-open denial, once corroborated against the
/// save file's own mtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDenialReport {
    /// The denied path's mtime never moved while we watched: the save
    /// genuinely appears stuck. Escalate loudly.
    Stuck {
        path: String,
        /// Whether the denied file was observed to actually carry the
        /// read-only attribute -- only then is that specific remedy honest.
        read_only: bool,
    },
    /// The denied path's mtime advanced after the denial was logged: the
    /// game is still saving, just not through the path the fs log shows.
    /// Worth a quiet note, not an alarm.
    StillLanding { path: String },
}

#[derive(Debug)]
struct PendingDenial {
    path: String,
    mtime_at_denial: Option<SystemTime>,
    polls_waited: u32,
}

#[derive(Debug)]
pub struct SaveIdentityTracker {
    path: PathBuf,
    cursor: u64,
    remainder: String,
    current: Option<String>,
    /// A denial observed this attachment, awaiting corroboration.
    pending_denial: Option<PendingDenial>,
    /// The corroborated outcome, once settled, waiting to be taken.
    settled_denial: Option<WriteDenialReport>,
    reported_denial: bool,
}

impl SaveIdentityTracker {
    pub fn after_current_log(path: &Path) -> Result<Self> {
        let cursor = std::fs::metadata(path)
            .with_context(|| format!("reading shad log metadata {}", path.display()))?
            .len();
        Ok(Self {
            path: path.to_owned(),
            cursor,
            remainder: String::new(),
            current: None,
            pending_denial: None,
            settled_denial: None,
            reported_denial: false,
        })
    }

    /// The corroborated outcome of a save write-open denial, reported once
    /// per attachment (again after a log truncation). A denial that settles
    /// as `Stuck` is invisible in-game until the player dies and loses
    /// progress, so the client says it out loud; one that settles as
    /// `StillLanding` is corroborated benign and only worth a quiet line.
    pub fn take_write_denial(&mut self) -> Option<WriteDenialReport> {
        if self.reported_denial {
            return None;
        }
        let report = self.settled_denial.take()?;
        self.reported_denial = true;
        Some(report)
    }

    /// Consume newly appended complete lines and return the latest freshly
    /// written character slot. Log truncation clears the old identity.
    pub fn poll(&mut self) -> Result<Option<String>> {
        let mut file = File::open(&self.path)
            .with_context(|| format!("opening shad log {}", self.path.display()))?;
        let length = file.metadata()?.len();
        if length < self.cursor {
            self.cursor = 0;
            self.remainder.clear();
            self.current = None;
            self.pending_denial = None;
            self.settled_denial = None;
            self.reported_denial = false;
        }
        file.seek(SeekFrom::Start(self.cursor))?;
        let mut appended = String::new();
        file.read_to_string(&mut appended)?;
        // Record what was actually consumed. The log can grow between the
        // metadata read and `read_to_string`; using the earlier length would
        // replay those extra bytes on the next poll.
        self.cursor = file.stream_position()?;
        self.remainder.push_str(&appended);

        let complete = self
            .remainder
            .rfind('\n')
            .map(|last| self.remainder[..=last].to_owned())
            .unwrap_or_default();
        if !complete.is_empty() {
            self.remainder.drain(..complete.len());
            for line in complete.lines() {
                if let Some(identity) = identity_from_write_line(line) {
                    self.current = Some(identity);
                }
                if let Some(path) = denied_save_write(line)
                    && self.pending_denial.is_none()
                    && self.settled_denial.is_none()
                {
                    let mtime_at_denial = file_mtime(Path::new(&path));
                    self.pending_denial = Some(PendingDenial {
                        path,
                        mtime_at_denial,
                        polls_waited: 0,
                    });
                }
            }
        }
        self.settle_pending_denial();
        Ok(self.current.clone())
    }

    /// Advance the corroboration window for a pending denial by one poll,
    /// resolving it into `settled_denial` once the save's own mtime either
    /// proves it is still landing or the settle window runs out.
    fn settle_pending_denial(&mut self) {
        let Some(pending) = self.pending_denial.as_mut() else {
            return;
        };
        let current_mtime = file_mtime(Path::new(&pending.path));
        let advanced = match (pending.mtime_at_denial, current_mtime) {
            (Some(before), Some(after)) => after > before,
            // No mtime at denial time (file did not exist yet) but it exists
            // now: something wrote it since. That is corroboration too.
            (None, Some(_)) => true,
            _ => false,
        };
        if advanced {
            let pending = self.pending_denial.take().unwrap();
            self.settled_denial = Some(WriteDenialReport::StillLanding { path: pending.path });
            return;
        }
        pending.polls_waited = pending.polls_waited.saturating_add(1);
        if pending.polls_waited >= DENIAL_SETTLE_POLLS {
            let pending = self.pending_denial.take().unwrap();
            let read_only = file_is_read_only(Path::new(&pending.path));
            self.settled_denial = Some(WriteDenialReport::Stuck {
                path: pending.path,
                read_only,
            });
        }
    }

    pub fn clear(&mut self) {
        self.current = None;
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn file_is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().readonly())
        .unwrap_or(false)
}

/// A host-side open failure on a save-data file: the emulator asked Windows
/// for the file and was refused. Only save-data paths count; the game also
/// probes optional content files that legitimately do not exist.
fn denied_save_write(line: &str) -> Option<String> {
    let rest = line.split_once(HOST_OPEN_FAILURE)?.1;
    if !rest.contains("savedata") {
        return None;
    }
    let path = rest.split(", error_message=").next()?.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

fn identity_from_write_line(line: &str) -> Option<String> {
    let suffix = line.split_once(PREFIX)?.1;
    let digits = suffix.get(..4)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) || !suffix.contains(WRITE_FLAGS) {
        return None;
    }
    let slot = digits.parse::<u8>().ok()?;
    (slot <= 9).then(|| format!("shad-save-slot:{slot:04}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "bb-save-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn append_line(log: &Path, line: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(log).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    /// clients#619: a denied write-open whose save file's mtime moves during
    /// the settle window must be reported quietly, never as data loss.
    #[test]
    fn a_denial_corroborated_by_a_changing_save_mtime_settles_as_still_landing() {
        let root = temp_root("still-landing");
        let log = root.join("shad_log.txt");
        let save = root.join("savedata").join("userdata0003");
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"initial").unwrap();
        std::fs::write(&log, "").unwrap();
        let mut tracker = SaveIdentityTracker::after_current_log(&log).unwrap();

        append_line(
            &log,
            &format!(
                "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path={}, error_message=permission denied",
                save.display()
            ),
        );
        tracker.poll().unwrap();
        assert!(tracker.take_write_denial().is_none(), "still corroborating");

        // The save file changes on disk even though the open was denied --
        // exactly the maintainer's observation of a benign always-failing probe.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&save, b"changed after the denial").unwrap();
        tracker.poll().unwrap();

        match tracker.take_write_denial() {
            Some(WriteDenialReport::StillLanding { path }) => {
                assert!(path.ends_with("userdata0003"), "{path}");
            }
            other => panic!("expected StillLanding, got {other:?}"),
        }
        // Single-report behaviour is unchanged.
        assert!(tracker.take_write_denial().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A denial whose file never changes across the settle window escalates
    /// to `Stuck`, and only claims the read-only remedy when it is true.
    #[test]
    fn a_denial_with_no_corroborating_mtime_change_settles_as_stuck_without_false_readonly_claim() {
        let root = temp_root("stuck");
        let log = root.join("shad_log.txt");
        // The denied file never exists on the host, matching the maintainer's
        // report: "the files the game tries to create do not exist at that
        // moment". The read-only remedy cannot apply here.
        let save = root.join("savedata").join("userdata0004");
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&log, "").unwrap();
        let mut tracker = SaveIdentityTracker::after_current_log(&log).unwrap();

        let line = format!(
            "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path={}, error_message=permission denied",
            save.display()
        );
        for _ in 0..(DENIAL_SETTLE_POLLS as usize) {
            append_line(&log, &line);
            tracker.poll().unwrap();
        }

        match tracker.take_write_denial() {
            Some(WriteDenialReport::Stuck { path, read_only }) => {
                assert!(path.ends_with("userdata0004"), "{path}");
                assert!(!read_only, "file does not exist; must not claim read-only");
            }
            other => panic!("expected Stuck, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// When the denied file does exist and genuinely carries the read-only
    /// attribute, `Stuck` should say so.
    #[test]
    fn a_stuck_denial_reports_read_only_only_when_actually_observed() {
        let root = temp_root("readonly");
        let log = root.join("shad_log.txt");
        let save = root.join("savedata").join("userdata0005");
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"stale").unwrap();
        let mut permissions = std::fs::metadata(&save).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&save, permissions).unwrap();
        std::fs::write(&log, "").unwrap();
        let mut tracker = SaveIdentityTracker::after_current_log(&log).unwrap();

        let line = format!(
            "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path={}, error_message=permission denied",
            save.display()
        );
        for _ in 0..(DENIAL_SETTLE_POLLS as usize) {
            append_line(&log, &line);
            tracker.poll().unwrap();
        }

        match tracker.take_write_denial() {
            Some(WriteDenialReport::Stuck { read_only, .. }) => assert!(read_only),
            other => panic!("expected Stuck, got {other:?}"),
        }

        // Clean up: clear read-only before removing the tree. This is a
        // Windows-only test helper resetting a local temp file's attribute,
        // not a Unix permission grant.
        let mut permissions = std::fs::metadata(&save).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&save, permissions).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_denial_is_reported_once_and_does_not_disturb_identity() {
        let root = temp_root("once");
        let log = root.join("shad_log.txt");
        std::fs::write(&log, "").unwrap();
        let mut tracker = SaveIdentityTracker::after_current_log(&log).unwrap();
        std::fs::write(&log, concat!(
            "[Kernel.Fs] <Info> (SLSession) file_system.cpp:81 open: path = /savedata0/userdata0003 flags = 0x201 mode = 0777\n",
            "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path=C:/Users/x/AppData/Roaming/shadPS4/home\\1000\\savedata\\CUSA00207\\SPRJ0005\\userdata0003, error_message=permission denied\n",
            "[Kernel.Fs] <Error> (Core.Res.CacheableFileLoader) file_system.cpp:163 open: Opening path /app0/dvdroot_ps4/map/m24/m24_9999.tpf.dcx failed, file does not exist\n",
        )).unwrap();
        assert_eq!(
            tracker.poll().unwrap().as_deref(),
            Some("shad-save-slot:0003")
        );
        // Not settled yet (file never existed, but settle window has only
        // seen one poll); drive the remaining polls to force a Stuck verdict.
        for _ in 1..(DENIAL_SETTLE_POLLS as usize) {
            tracker.poll().unwrap();
        }
        let denied = tracker.take_write_denial().unwrap();
        match denied {
            WriteDenialReport::Stuck { path, .. } => {
                assert!(path.ends_with("userdata0003"), "{path}")
            }
            other => panic!("expected Stuck, got {other:?}"),
        }
        assert!(tracker.take_write_denial().is_none());
        // A later denial in the same attachment is not re-reported...
        let mut appended = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut appended,
            b"[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path=C:/x/savedata/userdata0010, error_message=permission denied\n").unwrap();
        tracker.poll().unwrap();
        assert!(tracker.take_write_denial().is_none());
        // ...but a truncated log is a new emulator run.
        std::fs::write(&log, "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path=C:/x/savedata/userdata0003, error_message=permission denied\n").unwrap();
        for _ in 0..(DENIAL_SETTLE_POLLS as usize) {
            tracker.poll().unwrap();
        }
        assert!(tracker.take_write_denial().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_character_write_opens_identify_a_save() {
        assert_eq!(
            identity_from_write_line(
                "[Kernel.Fs] open: path = /savedata0/userdata0003 flags = 0x201 mode = 0777"
            ),
            Some("shad-save-slot:0003".into())
        );
        assert_eq!(
            identity_from_write_line(
                "[Kernel.Fs] open: path = /savedata0/userdata0003 flags = 0x0 mode = 0555"
            ),
            None
        );
        assert_eq!(
            identity_from_write_line(
                "[Kernel.Fs] open: path = /savedata0/userdata0010 flags = 0x201 mode = 0777"
            ),
            None
        );
    }

    #[test]
    fn tracker_ignores_history_and_follows_new_writes_and_truncation() {
        let root = temp_root("identity");
        let path = root.join("shad.log");
        std::fs::write(
            &path,
            "open: path = /savedata0/userdata0001 flags = 0x201 mode = 0777\n",
        )
        .unwrap();
        let mut tracker = SaveIdentityTracker::after_current_log(&path).unwrap();
        assert_eq!(tracker.poll().unwrap(), None);

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "open: path = /savedata0/userdata0007 flags = 0x201 mode = 0777"
        )
        .unwrap();
        assert_eq!(tracker.poll().unwrap(), Some("shad-save-slot:0007".into()));

        std::fs::write(&path, "short\n").unwrap();
        assert_eq!(tracker.poll().unwrap(), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
