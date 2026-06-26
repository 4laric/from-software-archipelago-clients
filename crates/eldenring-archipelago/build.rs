//! Build stamp: bake the git SHA (+ `-dirty`) and a UTC build timestamp into the binary so the
//! runtime connect banner identifies EXACTLY which build produced a log. Without this the only
//! version string is `CARGO_PKG_VERSION`, which is identical across rebuilds — meaning a logfile
//! can't be tied to a build, which is how we ended up debugging blind.
//!
//! No `rerun-if-changed` lines are emitted on purpose: that makes Cargo re-run this script whenever
//! any file in the crate changes (the default), so the SHA/dirty flag + timestamp refresh on every
//! meaningful rebuild — including uncommitted ("dirty") iteration, which is the common case here.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };

    let sha = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "nogit".to_string());
    // Any porcelain output (staged or unstaged) => the working tree differs from HEAD.
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=ER_GIT_SHA={sha}");

    let build_time = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    println!("cargo:rustc-env=ER_BUILD_TIME={build_time}");
}
