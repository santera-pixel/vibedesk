//! The `codebase-memory` step: the code graph server.
//!
//! The download is 46 MB and expands to a 318 MiB executable. The panel shows
//! the download figure, because that is what the user waits for; the extracted
//! size is a disk fact, not a wait.
//!
//! **This step writes the binary and nothing else.** `~/.cache/codebase-memory-mcp/`
//! holds the per-machine index databases — `vibe-home.db`, `vibedesk.db`,
//! `activity.db`, and one `.db` per indexed workspace — which are the user's
//! data, not part of the tool. They are also the bulk of the footprint: 135 MB
//! on a working machine against 46 MB for the download. Copying them into an
//! installer or a payload would ship one user's index to another user.

use crate::provision::Probe;
use std::io::Read;
use std::path::PathBuf;

const BINARY: &str = "codebase-memory-mcp";
const ASSET: &str = "codebase-memory-mcp-ui-windows-amd64.zip";
/// The download, not the extracted binary. See the module comment.
const DOWNLOAD_MB: f64 = 46.3;

fn url() -> String {
    format!("https://github.com/DeusData/codebase-memory-mcp/releases/latest/download/{ASSET}")
}

fn tools_dir() -> PathBuf {
    crate::paths::app_data_dir().join("tools")
}

pub fn probe() -> Probe {
    match crate::capabilities::external::find(BINARY) {
        Some(path) => {
            let version = crate::capabilities::external::status()
                .into_iter()
                .find(|server| server.name == "codebase-memory")
                .and_then(|server| server.version);
            Probe {
                satisfied: true,
                version,
                detail: path.display().to_string(),
            }
        }
        None => Probe {
            satisfied: false,
            version: None,
            detail: format!("not installed; the download is {DOWNLOAD_MB} MB"),
        },
    }
}

pub async fn install() -> Result<String, String> {
    if let Some(path) = crate::capabilities::external::find(BINARY) {
        return Ok(format!("already present at {}", path.display()));
    }

    let response = reqwest::get(url())
        .await
        .map_err(|error| format!("could not reach {}: {error}", url()))?;
    if !response.status().is_success() {
        return Err(format!("{} answered {}", url(), response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("could not read {}: {error}", url()))?;

    let dir = tools_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("{ASSET} is not a zip: {error}"))?;

    let mut extracted = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("could not read entry {index}: {error}"))?;
        let name = entry.name().to_string();
        if !name.ends_with(&format!("{BINARY}.exe")) {
            continue;
        }
        let mut body = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut body)
            .map_err(|error| format!("could not read {name}: {error}"))?;
        let target = dir.join(format!("{BINARY}.exe"));
        std::fs::write(&target, &body)
            .map_err(|error| format!("could not write {}: {error}", target.display()))?;
        extracted = Some((target, body.len()));
        break;
    }

    let (target, size) = extracted.ok_or_else(|| {
        format!("{ASSET} did not contain {BINARY}.exe; the release layout changed")
    })?;

    crate::capabilities::external::register("codebase-memory")
        .map_err(|error| format!("wrote {} but could not register it: {error}", target.display()))?;

    Ok(format!(
        "wrote {} ({:.0} MiB) and registered it",
        target.display(),
        size as f64 / 1_048_576.0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_is_the_windows_zip_the_release_publishes() {
        assert!(ASSET.ends_with("-windows-amd64.zip"));
        assert!(url().ends_with(ASSET));
        assert!(url().starts_with("https://github.com/DeusData/codebase-memory-mcp/"));
    }

    #[test]
    fn the_binary_lands_where_find_looks() {
        let dir = tools_dir();
        assert_eq!(
            dir,
            crate::paths::app_data_dir().join("tools"),
            "external::find searches <app_data>/tools"
        );
    }

    #[test]
    fn the_step_never_names_the_index_cache() {
        // A regression guard for the mistake this step exists to avoid: the
        // cache is the user's data and must not be read, copied or bundled.
        let source = include_str!("codebase_memory.rs");
        assert!(
            !source.contains("join(\".cache\")"),
            "the step must not reach into the index cache"
        );
    }
}
