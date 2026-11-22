//! Layout as data: widths, sidebar side, density.
//!
//! The theme module accepts CSS values, which is why it needs a rejection list.
//! A layout accepts **no CSS at all** — only integers and closed enums. There is
//! no string here that could become a declaration, so the worst a bad layout can
//! do is look wrong, and even that is bounded: every number is clamped into a
//! range that keeps the transcript and the composer usable.
//!
//! Clamping is silent on purpose. The UI reads the effective values back from
//! this module, so a slider that asked for 900px shows 520px — the correction is
//! visible without an error message.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A layout file larger than this is a mistake, not a layout.
pub const MAX_LAYOUT_BYTES: u64 = 8 * 1024;

/// Inclusive bounds for every numeric field. Anything outside is clamped.
/// Public so the manifest can publish the same numbers the clamp uses.
pub const RAIL_WIDTH: (u16, u16) = (40, 72);
pub const SIDEBAR_WIDTH: (u16, u16) = (200, 520);
pub const TRANSCRIPT_MAX: (u16, u16) = (480, 1100);
pub const COMPOSER_MAX: (u16, u16) = (480, 1100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    Compact,
    Comfortable,
    Roomy,
}

/// How much motion the app is allowed to use.
///
/// `System` is the default and the only value that reads the OS. The other two
/// exist because the OS setting is a blunt instrument: a user who turned off
/// "Animation effects" in Windows for battery reasons still wants a theme
/// reveal, and a user who wants no motion at all should not have to change a
/// machine-wide preference to get it. Neither value can *add* motion the
/// platform cannot render — `revealTheme` still falls back when
/// `startViewTransition` is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Motion {
    /// Follow `prefers-reduced-motion`.
    System,
    /// Animate regardless of the OS setting.
    Full,
    /// Never animate, regardless of the OS setting.
    Reduced,
}

impl Side {
    /// Both sides, for the manifest.
    pub const ALL: [Side; 2] = [Self::Left, Self::Right];
}

impl Density {
    /// All three densities, for the manifest.
    pub const ALL: [Density; 3] = [Self::Compact, Self::Comfortable, Self::Roomy];
}

impl Motion {
    /// All three motion preferences, for the manifest.
    pub const ALL: [Motion; 3] = [Self::System, Self::Full, Self::Reduced];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayoutConfig {
    /// Width of the icon rail, in CSS pixels.
    pub rail_width: u16,
    /// Width of the workstation rail, in CSS pixels.
    pub sidebar_width: u16,
    /// Which edge the workstation rail sits on.
    pub sidebar_side: Side,
    /// Collapse the workstation rail to nothing, keeping the toggle reachable.
    pub sidebar_collapsed: bool,
    /// Widest the transcript column is allowed to grow.
    pub transcript_max: u16,
    /// Widest the composer is allowed to grow.
    pub composer_max: u16,
    /// Vertical rhythm multiplier for the transcript.
    pub density: Density,
    /// How much motion the app may use. See [`Motion`].
    pub motion: Motion,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            rail_width: 48,
            sidebar_width: 252,
            sidebar_side: Side::Right,
            sidebar_collapsed: false,
            transcript_max: 720,
            composer_max: 720,
            density: Density::Comfortable,
            motion: Motion::System,
        }
    }
}

impl LayoutConfig {
    /// Pull every number into its allowed range. Idempotent.
    pub fn clamped(mut self) -> Self {
        self.rail_width = self.rail_width.clamp(RAIL_WIDTH.0, RAIL_WIDTH.1);
        self.sidebar_width = self.sidebar_width.clamp(SIDEBAR_WIDTH.0, SIDEBAR_WIDTH.1);
        self.transcript_max = self.transcript_max.clamp(TRANSCRIPT_MAX.0, TRANSCRIPT_MAX.1);
        self.composer_max = self.composer_max.clamp(COMPOSER_MAX.0, COMPOSER_MAX.1);
        self
    }
}

pub fn layout_path() -> PathBuf {
    super::customize::config_dir().join("layout.json")
}

/// Read the layout from disk. A missing file is the default layout, not an error.
pub fn load_layout() -> Result<LayoutConfig, String> {
    load_layout_at(&layout_path(), super::customize::safe_mode())
}

/// The safe-mode decision is a parameter so the bypass can be tested without
/// writing to the real data directory.
pub fn load_layout_at(path: &std::path::Path, safe: bool) -> Result<LayoutConfig, String> {
    if safe {
        return Ok(LayoutConfig::default());
    }
    load_layout_from(path)
}

pub fn load_layout_from(path: &std::path::Path) -> Result<LayoutConfig, String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(LayoutConfig::default());
    };
    if meta.len() > MAX_LAYOUT_BYTES {
        return Err(format!(
            "layout.json is {} bytes; the limit is {}",
            meta.len(),
            MAX_LAYOUT_BYTES
        ));
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(LayoutConfig::default());
    }

    let layout: LayoutConfig = serde_json::from_str(&raw)
        .map_err(|e| format!("layout.json is not valid JSON: {e}"))?;
    Ok(layout.clamped())
}

/// Clamp, then write. An out-of-range value never reaches disk.
pub fn save_layout(layout: &LayoutConfig) -> Result<LayoutConfig, String> {
    save_layout_to(&layout_path(), layout)
}

pub fn save_layout_to(
    path: &std::path::Path,
    layout: &LayoutConfig,
) -> Result<LayoutConfig, String> {
    let layout = layout.clone().clamped();

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    let body = serde_json::to_string_pretty(&layout)
        .map_err(|e| format!("could not serialise the layout: {e}"))?;

    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &body)
        .map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|e| format!("could not replace {}: {e}", path.display()))?;

    Ok(layout)
}

pub fn reset_layout() -> Result<LayoutConfig, String> {
    reset_layout_at(&layout_path())
}

pub fn reset_layout_at(path: &std::path::Path) -> Result<LayoutConfig, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(LayoutConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_inside_their_ranges() {
        let layout = LayoutConfig::default();
        let clamped = layout.clone().clamped();
        assert_eq!(clamped.rail_width, layout.rail_width);
        assert_eq!(clamped.sidebar_width, layout.sidebar_width);
        assert_eq!(clamped.transcript_max, layout.transcript_max);
        assert_eq!(clamped.composer_max, layout.composer_max);
    }

    #[test]
    fn clamps_out_of_range_numbers() {
        let layout = LayoutConfig {
            rail_width: 400,
            sidebar_width: 5,
            transcript_max: 60_000,
            composer_max: 1,
            ..LayoutConfig::default()
        }
        .clamped();

        assert_eq!(layout.rail_width, RAIL_WIDTH.1);
        assert_eq!(layout.sidebar_width, SIDEBAR_WIDTH.0);
        assert_eq!(layout.transcript_max, TRANSCRIPT_MAX.1);
        assert_eq!(layout.composer_max, COMPOSER_MAX.0);
    }

    #[test]
    fn clamping_is_idempotent() {
        let once = LayoutConfig {
            sidebar_width: 9_000,
            ..LayoutConfig::default()
        }
        .clamped();
        assert_eq!(once.sidebar_width, once.clone().clamped().sidebar_width);
    }

    #[test]
    fn rejects_unknown_enum_values() {
        let dir = std::env::temp_dir().join(format!("vibedesk-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");

        std::fs::write(&path, r#"{"sidebarSide":"diagonal"}"#).unwrap();
        assert!(load_layout_from(&path).is_err());

        std::fs::write(&path, r#"{"sidebarSide":"left","density":"roomy"}"#).unwrap();
        let layout = load_layout_from(&path).unwrap();
        assert_eq!(layout.sidebar_side, Side::Left);
        assert_eq!(layout.density, Density::Roomy);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The motion preference is a closed enum, defaults to following the OS,
    /// and survives a round trip through disk.
    #[test]
    fn motion_defaults_to_system_and_round_trips() {
        assert_eq!(LayoutConfig::default().motion, Motion::System);

        let dir = std::env::temp_dir().join(format!("vibedesk-layout-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");

        std::fs::write(&path, r#"{"motion":"full"}"#).unwrap();
        assert_eq!(load_layout_from(&path).unwrap().motion, Motion::Full);

        std::fs::write(&path, r#"{"motion":"reduced"}"#).unwrap();
        assert_eq!(load_layout_from(&path).unwrap().motion, Motion::Reduced);

        // A partial file that says nothing about motion keeps the default.
        std::fs::write(&path, r#"{"sidebarWidth":300}"#).unwrap();
        assert_eq!(load_layout_from(&path).unwrap().motion, Motion::System);

        // Anything outside the enum is a rejected file, not a silent default.
        std::fs::write(&path, r#"{"motion":"cinematic"}"#).unwrap();
        assert!(load_layout_from(&path).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_the_default_layout() {        let path = std::env::temp_dir().join("vibedesk-layout-does-not-exist.json");
        std::fs::remove_file(&path).ok();
        assert_eq!(load_layout_from(&path).unwrap().sidebar_width, 252);
    }

    #[test]
    fn partial_files_keep_the_other_defaults() {
        let dir = std::env::temp_dir().join(format!("vibedesk-layout-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");

        std::fs::write(&path, r#"{"sidebarWidth":300}"#).unwrap();
        let layout = load_layout_from(&path).unwrap();
        assert_eq!(layout.sidebar_width, 300);
        assert_eq!(layout.rail_width, 48);
        assert_eq!(layout.sidebar_side, Side::Right);

        std::fs::remove_dir_all(&dir).ok();
    }
}
