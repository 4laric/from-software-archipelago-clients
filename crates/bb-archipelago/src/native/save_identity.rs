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

#[derive(Debug)]
pub struct SaveIdentityTracker {
    path: PathBuf,
    cursor: u64,
    remainder: String,
    current: Option<String>,
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
        })
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
            }
        }
        Ok(self.current.clone())
    }

    pub fn clear(&mut self) {
        self.current = None;
    }
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
