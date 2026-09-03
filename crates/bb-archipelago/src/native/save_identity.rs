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

use anyhow::{Context, Result};

const PREFIX: &str = "open: path = /savedata0/userdata";
const WRITE_FLAGS: &str = "flags = 0x201";
/// shadPS4's host-side open failure, logged right after a save write-open
/// the emulator could not honour. Seen live (2026-09-03): every save of a
/// session denied because the userdata files were not writable, so the game
/// silently reverted to its last good save on death.
const HOST_OPEN_FAILURE: &str = "Failed to open the file at path=";

#[derive(Debug)]
pub struct SaveIdentityTracker {
    path: PathBuf,
    cursor: u64,
    remainder: String,
    current: Option<String>,
    /// The host path of the most recent denied save write, until reported.
    denied_write: Option<String>,
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
            denied_write: None,
            reported_denial: false,
        })
    }

    /// The host path of a save file shadPS4 could not open for writing,
    /// reported once per attachment (again after a log truncation). A denied
    /// save is invisible in-game until the player dies and loses progress,
    /// so the client says it out loud.
    pub fn take_write_denial(&mut self) -> Option<String> {
        if self.reported_denial {
            return None;
        }
        let denied = self.denied_write.take()?;
        self.reported_denial = true;
        Some(denied)
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
            self.denied_write = None;
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
                if let Some(path) = denied_save_write(line) {
                    self.denied_write = Some(path);
                }
            }
        }
        Ok(self.current.clone())
    }

    pub fn clear(&mut self) {
        self.current = None;
    }
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

    #[test]
    fn a_denied_save_write_is_reported_once_and_does_not_disturb_identity() {
        let root = std::env::temp_dir().join(format!(
            "bb-save-denied-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
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
        let denied = tracker.take_write_denial().unwrap();
        assert!(denied.ends_with("userdata0003"), "{denied}");
        assert!(tracker.take_write_denial().is_none());
        // A later denial in the same attachment is not re-reported...
        let mut appended = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut appended,
            b"[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path=C:/x/savedata/userdata0010, error_message=permission denied\n").unwrap();
        tracker.poll().unwrap();
        assert!(tracker.take_write_denial().is_none());
        // ...but a truncated log is a new emulator run.
        std::fs::write(&log, "[Common.Filesystem] <Error> (SLSession) io_file.cpp:202 Open: Failed to open the file at path=C:/x/savedata/userdata0003, error_message=permission denied\n").unwrap();
        tracker.poll().unwrap();
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
        let root = std::env::temp_dir().join(format!(
            "bb-save-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
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
