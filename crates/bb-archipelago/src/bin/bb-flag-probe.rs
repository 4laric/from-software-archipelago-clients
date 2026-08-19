use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bb_archipelago::event_flags::LiveEventFlags;

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let Some(shad_log) = arguments.next().map(PathBuf::from) else {
        bail!("usage: bb-flag-probe SHAD_LOG EVENT_FLAG [OUTPUT]")
    };
    let event_flag = arguments
        .next()
        .context("missing EVENT_FLAG")?
        .parse::<u32>()
        .context("EVENT_FLAG must be a decimal integer")?;
    let output = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        bail!("usage: bb-flag-probe SHAD_LOG EVENT_FLAG [OUTPUT]")
    }

    let flags = LiveEventFlags::attach(&shad_log)?;
    let result = format!("event_flag={event_flag} set={}\n", flags.read(event_flag)?);
    if let Some(output) = output {
        fs::write(&output, result).with_context(|| format!("writing {}", output.display()))?;
    } else {
        print!("{result}");
    }
    Ok(())
}
