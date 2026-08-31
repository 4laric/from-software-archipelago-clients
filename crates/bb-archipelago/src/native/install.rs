//! Static payload install with a thread-suspend atomicity protocol.
//!
//! The Python prototype's `install()` deliberately stopped short of atomicity
//! ("Suspending the guest threads and checking their instruction pointers
//! against the patch window is required before this is allowed anywhere near a
//! player, and is deliberately not implemented in a prototype that cannot be
//! tested"). This module implements exactly that protocol, with the Windows
//! thread primitives behind [`ThreadController`] and a scripted fake so every
//! branch is host-testable.
//!
//! The install order and the hazard both come from the contract. The state
//! region and the two caves are written first: they land in memory the image
//! asserts proved unused, so no live thread can be executing there. The two
//! seven-byte `E9 rel32 + NOP` detours are written **last and together, under a
//! single suspend of every guest thread**, only once no thread's instruction
//! pointer lies inside either detour's byte window. A detour that landed before
//! its cave, or across a thread mid-fetch, is the crash the heartbeat hook's
//! every-frame execution makes likely.
//!
//! Failure is closed: if the window cannot be cleared within the retry budget,
//! **no detour is written**, any half-written detour is rolled back to its
//! original bytes, and the threads are always resumed.

use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::contract::Contract;
use super::mem::{ProcessMemory, require_validated_image};

/// Windows thread control, behind a trait so the install protocol is testable
/// without a live process. All three are scoped to the target (shadPS4)
/// process.
pub trait ThreadController {
    /// Suspend every thread in the target process; returns how many were
    /// suspended.
    fn suspend_all(&mut self) -> Result<usize>;
    /// Resume every thread suspended by the matching [`Self::suspend_all`].
    fn resume_all(&mut self) -> Result<()>;
    /// The instruction pointer (RIP) of every thread. Only meaningful while the
    /// threads are suspended.
    fn instruction_pointers(&mut self) -> Result<Vec<u64>>;
}

/// Tunables for the install retry loop.
#[derive(Clone, Copy, Debug)]
pub struct InstallConfig {
    /// How many suspend/check cycles to attempt before failing closed.
    pub max_clear_attempts: u32,
    /// How long to let the guest run between cycles when a RIP sat in a window.
    pub nudge_delay: Duration,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            max_clear_attempts: 64,
            nudge_delay: Duration::from_millis(2),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOutcome {
    /// Blob names written, in order.
    pub written: Vec<String>,
    /// How many suspend cycles it took to clear the detour windows.
    pub suspend_cycles: u32,
}

fn is_detour(name: &str) -> bool {
    name.ends_with("_detour")
}

/// Install the full native payload at `base`. Fails closed on image mismatch
/// (via [`require_validated_image`]) and on an uncleared detour window.
///
/// `sleep` is injected so tests need not wait real time; production passes
/// `std::thread::sleep`.
pub fn install(
    memory: &impl ProcessMemory,
    base: u64,
    contract: &Contract,
    controller: &mut impl ThreadController,
    config: InstallConfig,
    mut sleep: impl FnMut(Duration),
) -> Result<InstallOutcome> {
    require_validated_image(memory, base, contract)?;

    let mut written = Vec::new();

    // 1. Data + caves first: unused memory the asserts proved zeroed.
    for blob in contract.blobs.iter().filter(|b| !is_detour(&b.name)) {
        let bytes = blob.relocated(base)?;
        memory
            .write(base + blob.rva, &bytes)
            .with_context(|| format!("writing {}", blob.name))?;
        written.push(blob.name.clone());
    }

    // 2. The detours, together, under one clear suspend.
    let detours: Vec<_> = contract
        .blobs
        .iter()
        .filter(|b| is_detour(&b.name))
        .collect();
    anyhow::ensure!(
        !detours.is_empty(),
        "contract has no detour blobs; refusing to arm nothing"
    );
    let windows: Vec<(u64, u64)> = detours
        .iter()
        .map(|d| (base + d.rva, base + d.rva + d.bytes.len() as u64))
        .collect();

    for attempt in 1..=config.max_clear_attempts {
        let count = controller
            .suspend_all()
            .context("suspending guest threads")?;
        // From here every path must resume before returning.
        let rips = match controller.instruction_pointers() {
            Ok(rips) => rips,
            Err(error) => {
                let _ = controller.resume_all();
                return Err(error).context("reading guest instruction pointers");
            }
        };
        let _ = count; // count is informational
        let obstructed = rips.iter().any(|rip| {
            windows
                .iter()
                .any(|(start, end)| *rip >= *start && *rip < *end)
        });
        if obstructed {
            controller
                .resume_all()
                .context("resuming after an obstructed window")?;
            if attempt == config.max_clear_attempts {
                bail!(
                    "aborting native install: a guest thread stayed inside a detour window for {attempt} attempts; no detour was written"
                );
            }
            sleep(config.nudge_delay);
            continue;
        }

        // Window clear: write both detours, then always resume.
        let mut write_result = Ok(());
        let mut detour_written: Vec<&super::contract::PayloadBlob> = Vec::new();
        for detour in &detours {
            match detour.relocated(base).and_then(|bytes| {
                memory
                    .write(base + detour.rva, &bytes)
                    .with_context(|| format!("writing {}", detour.name))
            }) {
                Ok(()) => detour_written.push(detour),
                Err(error) => {
                    write_result = Err(error);
                    break;
                }
            }
        }

        if let Err(error) = write_result {
            // Roll back any detour we managed to write so we never leave a
            // half-armed hook, then resume and fail.
            for detour in &detour_written {
                if let Ok(site) = contract.hook_site(detour_hook_name(&detour.name)) {
                    let _ = memory.write(base + detour.rva, &site.original_bytes);
                }
            }
            let _ = controller.resume_all();
            return Err(error).context("native detour install failed and was rolled back");
        }

        for detour in &detours {
            written.push(detour.name.clone());
        }
        controller
            .resume_all()
            .context("resuming after arming the detours")?;
        return Ok(InstallOutcome {
            written,
            suspend_cycles: attempt,
        });
    }

    unreachable!("the loop returns or bails on the final attempt")
}

/// The hook-site name a detour blob restores on rollback.
fn detour_hook_name(detour_name: &str) -> &str {
    match detour_name {
        "consume_detour" => "consume_return",
        "heartbeat_detour" => "idle_heartbeat",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::super::mem::FakeMemory;
    use super::*;

    /// A scripted controller: `rip_script` is a queue of RIP sets returned by
    /// successive `instruction_pointers()` calls. Counts suspend/resume calls.
    #[derive(Default)]
    struct FakeThreads {
        rip_script: Vec<Vec<u64>>,
        cursor: usize,
        suspends: u32,
        resumes: u32,
        currently_suspended: bool,
        // If true, threads are only ever considered suspended one-at-a-time --
        // used to assert both detours land inside one suspend/resume pair.
        max_concurrent_suspended: u32,
    }

    impl ThreadController for FakeThreads {
        fn suspend_all(&mut self) -> Result<usize> {
            assert!(!self.currently_suspended, "double suspend without resume");
            self.currently_suspended = true;
            self.suspends += 1;
            Ok(3)
        }
        fn resume_all(&mut self) -> Result<()> {
            assert!(self.currently_suspended, "resume without suspend");
            self.currently_suspended = false;
            self.resumes += 1;
            Ok(())
        }
        fn instruction_pointers(&mut self) -> Result<Vec<u64>> {
            assert!(self.currently_suspended, "RIP read while running");
            self.max_concurrent_suspended = self.max_concurrent_suspended.max(1);
            let rips = self
                .rip_script
                .get(self.cursor)
                .cloned()
                .unwrap_or_default();
            self.cursor += 1;
            Ok(rips)
        }
    }

    fn image(base: u64) -> FakeMemory {
        let memory = FakeMemory::new();
        let c = contract();
        for assert in &c.asserts {
            memory.store(base + assert.rva, &assert.bytes);
        }
        for name in ["consume_return", "idle_heartbeat"] {
            let site = c.hook_site(name).unwrap();
            memory.store(base + site.rva, &site.original_bytes);
        }
        memory
    }

    fn detour_addr(base: u64, name: &str) -> u64 {
        base + contract()
            .blobs
            .iter()
            .find(|b| b.name == name)
            .unwrap()
            .rva
    }

    #[test]
    fn a_clear_window_installs_both_detours_under_one_suspend() {
        let base = 0x4000_0000;
        let memory = image(base);
        let mut threads = FakeThreads {
            rip_script: vec![vec![0x1000, 0x2000, 0x3000]], // nowhere near a window
            ..Default::default()
        };
        let outcome = install(
            &memory,
            base,
            contract(),
            &mut threads,
            InstallConfig::default(),
            |_| panic!("clear window must not sleep"),
        )
        .unwrap();
        assert_eq!(outcome.suspend_cycles, 1);
        assert_eq!(threads.suspends, 1);
        assert_eq!(threads.resumes, 1, "threads must always be resumed");
        // Both detours plus the three data blobs were written.
        assert!(outcome.written.contains(&"consume_detour".to_string()));
        assert!(outcome.written.contains(&"heartbeat_detour".to_string()));
        // The E9 landed at both hook sites.
        assert_eq!(
            memory.read(detour_addr(base, "consume_detour"), 1).unwrap(),
            vec![0xE9]
        );
        assert_eq!(
            memory
                .read(detour_addr(base, "heartbeat_detour"), 1)
                .unwrap(),
            vec![0xE9]
        );
    }

    #[test]
    fn a_rip_in_the_window_nudges_then_succeeds() {
        let base = 0x4000_0000;
        let memory = image(base);
        let consume = detour_addr(base, "consume_detour");
        let mut threads = FakeThreads {
            // First cycle: a thread sits mid-detour. Second cycle: clear.
            rip_script: vec![vec![consume + 3], vec![0x9999]],
            ..Default::default()
        };
        let mut slept = 0u32;
        let outcome = install(
            &memory,
            base,
            contract(),
            &mut threads,
            InstallConfig::default(),
            |_| slept += 1,
        )
        .unwrap();
        assert_eq!(outcome.suspend_cycles, 2);
        assert_eq!(threads.suspends, 2);
        assert_eq!(threads.resumes, 2);
        assert_eq!(
            slept, 1,
            "one nudge between the obstructed and clear cycles"
        );
    }

    #[test]
    fn a_persistently_obstructed_window_aborts_with_no_detour_and_resumes() {
        let base = 0x4000_0000;
        let memory = image(base);
        let heartbeat = detour_addr(base, "heartbeat_detour");
        let attempts = 4u32;
        let mut threads = FakeThreads {
            // Every cycle: a thread sits inside the heartbeat detour window.
            rip_script: (0..attempts).map(|_| vec![heartbeat + 1]).collect(),
            ..Default::default()
        };
        let error = install(
            &memory,
            base,
            contract(),
            &mut threads,
            InstallConfig {
                max_clear_attempts: attempts,
                nudge_delay: Duration::ZERO,
            },
            |_| {},
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("no detour was written"));
        assert_eq!(threads.suspends, attempts);
        assert_eq!(
            threads.resumes, attempts,
            "every suspend is resumed on abort"
        );
        // Neither hook site was patched: originals intact.
        let consume_site = contract().hook_site("consume_return").unwrap();
        assert_eq!(
            memory
                .read(
                    detour_addr(base, "consume_detour"),
                    consume_site.original_bytes.len()
                )
                .unwrap(),
            consume_site.original_bytes
        );
        let hb_site = contract().hook_site("idle_heartbeat").unwrap();
        assert_eq!(
            memory
                .read(
                    detour_addr(base, "heartbeat_detour"),
                    hb_site.original_bytes.len()
                )
                .unwrap(),
            hb_site.original_bytes
        );
    }

    #[test]
    fn install_fails_closed_on_an_unvalidated_image() {
        let base = 0x4000_0000;
        let memory = image(base);
        // Corrupt one assert byte: install must refuse before touching threads.
        let rva = contract()
            .asserts
            .iter()
            .find(|a| a.name == "heartbeat_hook")
            .unwrap()
            .rva;
        memory.store(base + rva, &[0x00]);
        let mut threads = FakeThreads::default();
        let error = install(
            &memory,
            base,
            contract(),
            &mut threads,
            InstallConfig::default(),
            |_| {},
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("not the validated"));
        assert_eq!(
            threads.suspends, 0,
            "no thread was touched on image refusal"
        );
    }

    #[test]
    fn data_blobs_are_written_before_the_detours() {
        let base = 0x4000_0000;
        let memory = image(base);
        let mut threads = FakeThreads {
            rip_script: vec![vec![0x10]],
            ..Default::default()
        };
        let outcome = install(
            &memory,
            base,
            contract(),
            &mut threads,
            InstallConfig::default(),
            |_| {},
        )
        .unwrap();
        let consume_data = outcome
            .written
            .iter()
            .position(|n| n == "consume_cave")
            .unwrap();
        let consume_detour = outcome
            .written
            .iter()
            .position(|n| n == "consume_detour")
            .unwrap();
        assert!(consume_data < consume_detour, "caves precede detours");
    }
}
