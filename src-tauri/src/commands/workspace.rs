use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::commands::session::PromptImage;
use crate::error::{AppError, AppResult};
use crate::paths;
use crate::AppState;

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".idea",
    ".vscode",
    "vendor",
    ".cache",
    ".turbo",
    "coverage",
];

const MAX_SCANNED: usize = 12_000;
const MAX_DEPTH: usize = 8;
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// An image the agent has to accept, so it is bounded by what a model will
/// take rather than by what the disk will give. 8 MB of PNG is already far
/// past the point where the agent downsamples it.
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
}

async fn workspace_root(state: &AppState) -> PathBuf {
    state
        .bridge
        .cwd()
        .await
        .unwrap_or_else(paths::default_workspace)
}

fn resolve(root: &Path, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    }
}

/// Bounded recursive walk. The composer only needs a few dozen candidates, so
/// scanning stops early rather than indexing the whole tree.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<FileEntry>) {
    if depth > MAX_DEPTH || out.len() >= MAX_SCANNED {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_SCANNED {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        if name.starts_with('.') && name != ".vibe" && name != ".agents" && is_dir {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        out.push(FileEntry {
            path: relative,
            name,
            is_dir,
        });

        if is_dir {
            walk(root, &path, depth + 1, out);
        }
    }
}

fn score(entry: &FileEntry, needle: &str) -> Option<u32> {
    let name = entry.name.to_lowercase();
    let path = entry.path.to_lowercase();

    if name == needle {
        return Some(1000);
    }
    if name.starts_with(needle) {
        return Some(800);
    }
    if name.contains(needle) {
        return Some(600);
    }
    if path.contains(needle) {
        return Some(300);
    }

    // Subsequence match, so `cmp` finds `Composer.tsx`.
    let mut chars = name.chars();
    let mut matched = 0usize;
    for target in needle.chars() {
        if chars.any(|c| c == target) {
            matched += 1;
        } else {
            return None;
        }
    }
    (matched == needle.chars().count()).then_some(100)
}

#[tauri::command]
pub async fn workspace_files(
    state: State<'_, AppState>,
    query: String,
    limit: usize,
) -> AppResult<Vec<FileEntry>> {
    let root = workspace_root(&state).await;
    let mut entries = Vec::new();
    walk(&root, &root, 0, &mut entries);

    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.truncate(limit.clamp(1, 200));
        return Ok(entries);
    }

    let mut scored: Vec<(u32, FileEntry)> = entries
        .into_iter()
        .filter_map(|entry| score(&entry, &needle).map(|value| (value, entry)))
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    Ok(scored
        .into_iter()
        .take(limit.clamp(1, 200))
        .map(|(_, entry)| entry)
        .collect())
}

#[tauri::command]
pub async fn workspace_read(state: State<'_, AppState>, path: String) -> AppResult<String> {
    let root = workspace_root(&state).await;
    let target = resolve(&root, &path);

    let metadata = std::fs::metadata(&target)
        .map_err(|e| AppError::message(format!("could not open {}: {e}", target.display())))?;
    if metadata.len() > MAX_READ_BYTES {
        return Err(AppError::message(format!(
            "{} is larger than 2 MB; open it in your editor instead",
            target.display()
        )));
    }

    std::fs::read_to_string(&target)
        .map_err(|e| AppError::message(format!("could not read {}: {e}", target.display())))
}

#[tauri::command]
pub async fn workspace_apply_edit(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> AppResult<()> {
    let root = workspace_root(&state).await;
    let target = resolve(&root, &path);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::message(format!("could not create {}: {e}", parent.display()))
        })?;
    }

    std::fs::write(&target, content)
        .map_err(|e| AppError::message(format!("could not write {}: {e}", target.display())))
}

#[tauri::command]
pub async fn workspace_reveal(state: State<'_, AppState>, path: String) -> AppResult<()> {
    let root = workspace_root(&state).await;
    let target = resolve(&root, &path);

    if !target.exists() {
        return Err(AppError::message(format!(
            "{} does not exist",
            target.display()
        )));
    }

    tauri_plugin_opener::reveal_item_in_dir(&target)
        .map_err(|e| AppError::message(format!("could not reveal {}: {e}", target.display())))
}

/* ------------------------------------------------------------------ *
 * Images
 *
 * The composer can take an image three ways: pasted, dropped, or picked.
 * The first two arrive in the webview as a `File`, which it reads itself.
 * The third is a path from the native dialog, and a path is not bytes — so
 * this is the one place the backend has to open the file, name what it is,
 * and hand back something ACP will accept.
 * ------------------------------------------------------------------ */

/// Standard base64 with padding.
///
/// Hand-rolled rather than pulled in as a dependency: the only bytes this
/// crate ever encodes are one image the user picked, and a 20-line table is
/// cheaper than a crate to audit and keep current.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The mime type from the file's own magic bytes.
///
/// Not from the extension: a `.png` that is really a JPEG is rejected by the
/// agent, and the extension is the one thing about a file that is routinely
/// wrong. `None` means it is not an image this app will send.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

#[tauri::command]
pub async fn image_read(path: String) -> AppResult<PromptImage> {
    let target = PathBuf::from(&path);
    let metadata = std::fs::metadata(&target)
        .map_err(|e| AppError::message(format!("could not open {path}: {e}")))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(AppError::message(format!(
            "{path} is larger than 8 MB; attach a smaller image"
        )));
    }

    let bytes = std::fs::read(&target)
        .map_err(|e| AppError::message(format!("could not read {path}: {e}")))?;
    let mime_type = image_mime(&bytes)
        .ok_or_else(|| {
            AppError::message(format!("{path} is not a PNG, JPEG, GIF or WebP image"))
        })?
        .to_string();

    Ok(PromptImage {
        data: base64(&bytes),
        mime_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_each_remainder() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_covers_the_whole_alphabet() {
        // Every byte value, so no index is off by one and no arm is unreachable.
        let all: Vec<u8> = (0..=255u8).collect();
        let encoded = base64(&all);
        assert_eq!(encoded.len(), 344);
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        assert!(encoded.ends_with("29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w=="));
    }

    #[test]
    fn image_mime_reads_magic_not_extension() {
        assert_eq!(
            image_mime(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Some("image/png")
        );
        assert_eq!(image_mime(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(
            image_mime(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(image_mime(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        assert_eq!(image_mime(b"not an image at all"), None);
        assert_eq!(image_mime(b""), None);
    }
}
