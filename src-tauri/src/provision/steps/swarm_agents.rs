//! The `swarm-agents` step: write the vendored payload into `~/.vibe`.
//!
//! This is the step the whole plan exists for. Without it the lead runs on
//! Vibe's own coding prompt, which says "if the task requires an edit, edit",
//! and the lead implements instead of planning.
//!
//! Written content-addressed: read the destination, compare bytes, write only
//! when they differ. A matching file is left alone, so a user who edited one
//! keeps their edit until VibeDesk's payload changes — and then it is
//! overwritten, which the ledger records.

use crate::provision::payload::{self, Dir};
use crate::provision::Probe;
use std::path::PathBuf;

fn dir_for(dir: Dir) -> PathBuf {
    crate::paths::vibe_home().join(dir.name())
}

fn destination(file: &payload::File) -> PathBuf {
    dir_for(file.dir).join(file.name)
}

/// How many payload files are already on disk with the shipped bytes.
pub fn matching() -> usize {
    payload::FILES
        .iter()
        .filter(|file| {
            std::fs::read_to_string(destination(file))
                .map(|existing| existing == file.body)
                .unwrap_or(false)
        })
        .count()
}

pub fn probe() -> Probe {
    let total = payload::FILES.len();
    let matching = matching();
    if matching == total {
        return Probe {
            satisfied: true,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            detail: format!("{total} files match the payload"),
        };
    }
    Probe {
        satisfied: false,
        version: None,
        detail: format!("{matching} of {total} payload files match"),
    }
}

/// Write every payload file whose bytes differ, and report what changed.
pub fn install() -> Result<String, String> {
    let mut written = Vec::new();
    let mut kept = 0usize;

    for file in payload::FILES {
        let path = destination(file);
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if existing == file.body {
                kept += 1;
                continue;
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, file.body)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        // Named as the agent Vibe will advertise, not as the file, because the
        // agent name is what `roles::agent_for` selects and what the user sees in
        // the mode picker.
        written.push(match file.dir {
            Dir::Agents => payload::agent_name(file),
            Dir::Prompts => file.name,
        });
    }

    if written.is_empty() {
        return Ok(format!("{} files already match the payload", payload::FILES.len()));
    }
    Ok(format!(
        "wrote {} of {} files ({} already matched): {}",
        written.len(),
        payload::FILES.len(),
        kept,
        written.join(", ")
    ))
}

/// The files the swarm needs before it may start, as absolute paths.
pub fn required_paths() -> Vec<PathBuf> {
    payload::FILES.iter().map(destination).collect()
}

/// The first required file that is missing, for the start guard's message.
pub fn first_missing() -> Option<PathBuf> {
    required_paths().into_iter().find(|path| !path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_payload_file_has_a_destination_under_vibe_home() {
        let home = crate::paths::vibe_home();
        for file in payload::FILES {
            let path = destination(file);
            assert!(
                path.starts_with(&home),
                "{} must land under {}",
                path.display(),
                home.display()
            );
            let expected_dir = match file.dir {
                Dir::Agents => "agents",
                Dir::Prompts => "prompts",
            };
            assert_eq!(path.parent().unwrap().file_name().unwrap(), expected_dir);
        }
    }

    #[test]
    fn the_required_set_is_the_whole_payload() {
        assert_eq!(required_paths().len(), payload::FILES.len());
    }
}
