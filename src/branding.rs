//! Branding metadata shared by the in-app About (all entry points: native app menu, Help/fallback
//! menu), the window/icon setup and packaging. The version comes from Cargo metadata;
//! scripts/build-app.sh reads NAME, BUNDLE_ID, YEAR and AUTHOR from this file and the version
//! from Cargo.toml, so there is no second editable copy. Keep each `pub const` on one line.

pub const NAME: &str = "RocksVision";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const YEAR: &str = "2026";
pub const AUTHOR: &str = "TA2LDV";
/// macOS bundle identifier (Info.plist CFBundleIdentifier).
pub const BUNDLE_ID: &str = "com.nexir.rocksvision";

/// Approved logo (logo/rocksvision-cat-medium.png), used unmodified.
pub const LOGO_PNG: &[u8] = include_bytes!("../logo/rocksvision-cat-medium.png");

/// "RocksVision v0.1.0 - 2026"
pub fn about_title() -> String {
    format!("{NAME} v{VERSION} - {YEAR}")
}

/// "by TA2LDV"
pub fn about_byline() -> String {
    format!("by {AUTHOR}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn about_lines() {
        assert_eq!(super::about_title(), format!("RocksVision v{} - 2026", env!("CARGO_PKG_VERSION")));
        assert_eq!(super::about_byline(), "by TA2LDV");
        assert!(super::LOGO_PNG.starts_with(b"\x89PNG"));
    }
}
