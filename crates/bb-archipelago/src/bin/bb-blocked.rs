//! Parked-grant inspection and resolution (clients#399). A grant that
//! terminally fails in the harness is parked: acknowledged in order with its
//! failure detail, so the delivery stream keeps moving and the entry waits
//! here for an operator decision.
//!
//! Listing shows every parked entry with the manual re-grant hint. Resolving
//! is `INDEX --confirm`, which asserts "I verified the item physically
//! arrived" and clears the blocked marker. It never re-grants: re-issuing an
//! already-delivered item duplicates it. To actually re-deliver, use the
//! bb-archipelago repo's `tools/send_native_item_grant.ps1` with a fresh tag
//! (the harness state file must be cleared and the CE table re-run first),
//! then confirm here.
//!
//! usage: bb-blocked LEDGER SEED_NAME SLOT_NAME [INDEX --confirm]

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bb_archipelago::ledger::ReceiveLedger;

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let Some(ledger_path) = arguments.next().map(PathBuf::from) else {
        bail!("usage: bb-blocked LEDGER SEED_NAME SLOT_NAME [INDEX --confirm]")
    };
    let seed_name = arguments.next().context("missing SEED_NAME")?;
    let slot_name = arguments.next().context("missing SLOT_NAME")?;
    let confirm_index = match (arguments.next(), arguments.next()) {
        (None, None) => None,
        (Some(index), Some(flag)) if flag == "--confirm" => Some(
            index
                .parse::<u64>()
                .context("INDEX must be a decimal AP receive index")?,
        ),
        _ => bail!("usage: bb-blocked LEDGER SEED_NAME SLOT_NAME [INDEX --confirm]"),
    };
    if arguments.next().is_some() {
        bail!("usage: bb-blocked LEDGER SEED_NAME SLOT_NAME [INDEX --confirm]")
    }

    let mut ledger = ReceiveLedger::load(&ledger_path)
        .with_context(|| format!("loading receive ledger {}", ledger_path.display()))?;
    let slot = ledger.slot_mut(&seed_name, &slot_name);

    if let Some(index) = confirm_index {
        let detail = slot.unblock(index)?;
        ledger.save(&ledger_path)?;
        eprintln!(
            "Parked entry {index} in {seed_name}/{slot_name} confirmed as delivered \
             (was: {detail}); the ledger no longer treats it as blocked. \
             Nothing was re-granted."
        );
        return Ok(());
    }

    let blocked = slot.blocked_entries().collect::<Vec<_>>();
    if blocked.is_empty() {
        eprintln!("No parked items in {seed_name}/{slot_name}.");
        return Ok(());
    }
    for (index, item) in blocked {
        eprintln!(
            "index {index}: AP item {} | normalized {:#010X} x{} | {}",
            item.ap_item_id,
            item.normalized_item_id,
            item.quantity,
            item.blocked.as_deref().unwrap_or("blocked"),
        );
        eprintln!(
            "  to re-deliver: tools/send_native_item_grant.ps1 -RawId {:#010X} \
             -NormalizedId {:#010X} -Quantity {} -Tag ap_manual_{index} \
             (clear the harness state file and re-run the CE table first), \
             then: bb-blocked LEDGER {seed_name} {slot_name} {index} --confirm",
            item.raw_descriptor, item.normalized_item_id, item.quantity,
        );
        eprintln!(
            "  if it already arrived (the common torn-scan false failure): \
             bb-blocked LEDGER {seed_name} {slot_name} {index} --confirm",
        );
    }
    Ok(())
}
