//! Operator-attested save restore (bb-archipelago#77, SAVE-RECONCILIATION.md
//! §5 MVP). Asserts "I restored the save to before AP receive index K" and
//! rewinds the durable ledger cursor so indexes K.. are re-delivered in order
//! on the next client run.
//!
//! This is the honest manual stand-in for the save-resident watermark until
//! #56 supplies a writable save field: it asks the operator for the evidence
//! instead of guessing from inventory contents, which §4 forbids.
//!
//! usage: bb-restored LEDGER SEED_NAME SLOT_NAME FIRST_MISSING_INDEX

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bb_archipelago::ledger::ReceiveLedger;

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let Some(ledger_path) = arguments.next().map(PathBuf::from) else {
        bail!("usage: bb-restored LEDGER SEED_NAME SLOT_NAME FIRST_MISSING_INDEX")
    };
    let seed_name = arguments.next().context("missing SEED_NAME")?;
    let slot_name = arguments.next().context("missing SLOT_NAME")?;
    let first_missing = arguments
        .next()
        .context("missing FIRST_MISSING_INDEX")?
        .parse::<u64>()
        .context("FIRST_MISSING_INDEX must be a decimal AP receive index")?;
    if arguments.next().is_some() {
        bail!("usage: bb-restored LEDGER SEED_NAME SLOT_NAME FIRST_MISSING_INDEX")
    }

    let mut ledger = ReceiveLedger::load(&ledger_path)
        .with_context(|| format!("loading receive ledger {}", ledger_path.display()))?;
    let slot = ledger.slot_mut(&seed_name, &slot_name);
    let highest = slot.highest_processed_index;
    anyhow::ensure!(
        highest.is_some_and(|highest| first_missing <= highest),
        "nothing to rewind: the ledger has processed up to {highest:?}, \
         so a restore to before index {first_missing} is not a regression"
    );
    let rewound = slot.attest_restore(first_missing);
    ledger.save(&ledger_path)?;

    // The §8 operator line: say exactly what happens next, never silently.
    eprintln!(
        "Restore attested for {seed_name}/{slot_name}: save is at delivery {} of {}; \
         re-delivering {rewound} item(s) in order on the next client run.",
        first_missing,
        highest.map_or(0, |highest| highest + 1),
    );
    Ok(())
}
