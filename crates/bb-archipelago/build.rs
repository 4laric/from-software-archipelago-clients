use std::env;
use std::io;

const PRODUCT_NAME: &str = "Bloodborne Archipelago";
const PUBLISHER: &str = "4laric";
const COPYRIGHT: &str = "Copyright (c) 2026 4laric";

fn release_versions(raw: &str) -> io::Result<(String, String, u64, bool)> {
    let product = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    let (base, suffix) = product.split_once('-').unwrap_or((product, ""));
    let mut base_parts = base.split('.');
    let mut numeric = [0_u16; 4];
    for target in numeric.iter_mut().take(3) {
        let value = base_parts
            .next()
            .ok_or_else(|| io::Error::other("release version needs major.minor.patch"))?;
        *target = value
            .parse()
            .map_err(|_| io::Error::other("release version components must be 0..65535"))?;
    }
    if base_parts.next().is_some() {
        return Err(io::Error::other(
            "release version has too many numeric components",
        ));
    }
    if !suffix.is_empty() {
        numeric[3] = suffix
            .rsplit('.')
            .next()
            .ok_or_else(|| io::Error::other("prerelease needs a numeric sequence"))?
            .parse()
            .map_err(|_| io::Error::other("prerelease must end in a 0..65535 sequence"))?;
    }
    let file = numeric
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let encoded = (u64::from(numeric[0]) << 48)
        | (u64::from(numeric[1]) << 32)
        | (u64::from(numeric[2]) << 16)
        | u64::from(numeric[3]);
    Ok((product.to_owned(), file, encoded, !suffix.is_empty()))
}

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=BB_RELEASE_VERSION");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let raw = env::var("BB_RELEASE_VERSION")
        .unwrap_or_else(|_| format!("{}-dev.0", env::var("CARGO_PKG_VERSION").unwrap()));
    let (product_version, file_version, encoded, prerelease) = release_versions(&raw)?;
    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", PRODUCT_NAME)
        .set("FileDescription", "Bloodborne Archipelago Client")
        .set("CompanyName", PUBLISHER)
        .set("ProductVersion", &product_version)
        .set("FileVersion", &file_version)
        .set("OriginalFilename", "bb-ap-client.exe")
        .set("InternalName", "bb-ap-client.exe")
        .set("LegalCopyright", COPYRIGHT)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, encoded)
        .set_version_info(winresource::VersionInfo::FILEVERSION, encoded);
    if prerelease {
        resource.set_version_info(
            winresource::VersionInfo::FILEFLAGS,
            winresource::VersionInfo::VS_FF_PRERELEASE,
        );
    }
    resource.compile()
}

#[cfg(test)]
mod tests {
    use super::release_versions;

    #[test]
    fn release_tag_maps_to_windows_numeric_and_display_versions() {
        let (product, file, encoded, prerelease) = release_versions("v0.1.0-playtest.35").unwrap();
        assert_eq!(product, "0.1.0-playtest.35");
        assert_eq!(file, "0.1.0.35");
        assert_eq!(encoded, (1_u64 << 32) | 35);
        assert!(prerelease);
    }

    #[test]
    fn stable_version_uses_zero_revision() {
        let (product, file, _, prerelease) = release_versions("v1.2.3").unwrap();
        assert_eq!(product, "1.2.3");
        assert_eq!(file, "1.2.3.0");
        assert!(!prerelease);
    }
}
