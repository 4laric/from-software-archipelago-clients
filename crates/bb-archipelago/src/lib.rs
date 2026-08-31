//! Standalone Bloodborne Archipelago client components.

/// Exact cross-repository runtime contract. The apworld, native client, and
/// Cheat Engine harness bump this together whenever their shared shape changes.
pub const RUNTIME_BUILD: &str = "bb-0.1.0-r9";

/// Version that identifies the exact client binary, not merely its Cargo package version.
///
/// CI supplies `BB_BUILD_SHA`; local/offline builds deliberately remain identifiable as `dev`.
/// This is compile-time metadata only -- do not shell out to git from a build script, because
/// release consumers build this crate from source archives and other git-less contexts.
pub fn client_version() -> String {
    let sha = option_env!("BB_BUILD_SHA")
        .filter(|sha| !sha.is_empty())
        .unwrap_or("dev");
    let short = sha.get(..12).unwrap_or(sha);
    format!("{}+{short}", env!("CARGO_PKG_VERSION"))
}

pub mod backend;
pub mod bridge;
pub mod client_loop;
pub mod config;
pub mod event_flags;
pub mod feed;
pub mod ledger;
pub mod logging;
pub mod native;
pub mod upgrades;

#[cfg(test)]
mod tests {
    use super::client_version;

    #[test]
    fn binary_version_names_the_package_and_build() {
        let version = client_version();
        assert!(version.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")));
        assert!(version.is_ascii());
        assert!(version.len() <= env!("CARGO_PKG_VERSION").len() + 1 + 12);
    }
}
