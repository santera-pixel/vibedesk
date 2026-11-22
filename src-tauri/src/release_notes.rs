use serde::Serialize;

use crate::paths;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNote {
    pub version: String,
    pub body: String,
}

/// Release notes ship inside the Python package as `vibe/whats_new/*.md`.
///
/// Reading them from the installed package keeps VibeDesk honest: the notes
/// always describe the binary that is actually running, not a bundled copy.
pub fn load(limit: usize) -> Vec<ReleaseNote> {
    let Some(dir) = notes_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut notes: Vec<ReleaseNote> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                let version = path.file_stem()?.to_string_lossy().to_string();
                let body = std::fs::read_to_string(&path).ok()?;
                Some(ReleaseNote { version, body })
            } else {
                None
            }
        })
        .collect();

    notes.sort_by(|a, b| compare_versions(&b.version, &a.version));
    notes.truncate(limit);
    notes
}

fn notes_dir() -> Option<std::path::PathBuf> {
    let binary = paths::resolve_acp_binary().ok()?;
    let package = paths::vibe_package_dir(&binary.path)?;
    let dir = package.join("whats_new");
    dir.is_dir().then_some(dir)
}

/// Numeric-aware comparison so `3.10.0` sorts above `3.9.2`.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |value: &str| -> Vec<u64> {
        value
            .split(['.', '-', '+'])
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let left = parse(a);
    let right = parse(b);
    for index in 0..left.len().max(right.len()) {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        match l.cmp(&r) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// The newest note, used by the "what's new" affordance after an update.
pub fn latest() -> Option<ReleaseNote> {
    load(1).into_iter().next()
}
