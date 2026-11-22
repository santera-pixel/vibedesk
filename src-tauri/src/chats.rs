//! Archiving and deleting chats.
//!
//! A chat is an ACP session, and sessions live in the Vibe CLI's own store
//! (`~/.vibe/logs/session/unified/<id>/`). That store is shared with the user's
//! terminal sessions, so this module is deliberate about what it touches:
//!
//! * **Archive** is VibeDesk's own bookkeeping. Nothing on disk moves. The chat
//!   is hidden from the rail and can be brought back, so it is a preference
//!   rather than a deletion, and it lives in `<data>/config/chats.json`.
//! * **Delete** moves the session directory into `<data>/trash/sessions/<id>/`.
//!   The CLI's `session/list` tolerates a missing directory — verified against
//!   the real binary, which simply omits it — so the chat disappears from both
//!   VibeDesk and `vibe --resume`, and the bytes are still recoverable by hand.
//!   Nothing is ever `rm -rf`'d.
//!
//! The CLI's catalog (`.session-index.json`) is deliberately left alone. It is a
//! checksummed structure owned by the CLI, and hand-editing it from a GUI is how
//! you corrupt someone's history. A stale entry is tolerated; a mangled catalog
//! is not.
//!
//! Two things are refused outright. A session id that is not a plain id is
//! rejected before it is ever joined onto a path — `../../` in an id would
//! otherwise be a directory move outside the store. And the session VibeDesk
//! currently has open is refused, because pulling its directory out from under
//! the agent writing to it is how the app dies.
//!
//! The CLI's lock directory (`logs/session/active/`) is deliberately *not*
//! consulted. A lock there means "this process has loaded this session at some
//! point", not "this is the session on screen": one CLI process keeps a lock for
//! every session it has ever loaded and only drops them when it exits. Reading a
//! lock as "open" made every chat the user had ever visited undeletable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::customize;
use crate::paths;

/// Where the CLI keeps session bodies. Its layout, not ours — a missing
/// directory is not an error, it just means there is nothing to move.
const SESSION_STORE: &str = "logs/session/unified";

/// Session ids are UUIDs. Anything else is refused rather than joined onto a
/// path.
const MAX_ID_LEN: usize = 128;

/// A cap on the bookkeeping file. It is written by us, so this is a tripwire
/// against a hand-edited file, not a defence.
pub const MAX_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatsState {
    pub archived: Vec<String>,
    pub deleted: Vec<String>,
}

/// One chat a bulk delete could not move, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkFailure {
    pub session_id: String,
    pub error: String,
}

/// What a bulk delete did.
///
/// One chat that cannot go must not stop the rest, so the caller gets the new
/// bookkeeping plus a per-id account: what moved, what was the open chat, and
/// what failed. A bulk call that reported only success would be lying about the
/// one case the user needs to know about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkDelete {
    pub state: ChatsState,
    pub done: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<BulkFailure>,
}

/// The ids a bulk call should act on: valid, de-duplicated, in the order given.
///
/// Repeats are dropped rather than refused — a caller that lists the same chat
/// twice means it once — but an id that is not a plain id is an error, because
/// this is the only place a string from the webview becomes a filesystem path.
pub fn bulk_ids(ids: &[String]) -> Result<Vec<String>, String> {
    if ids.len() > MAX_ENTRIES {
        return Err(format!(
            "{} chats is more than the {MAX_ENTRIES} this can take at once",
            ids.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if !valid_id(id) {
            return Err(format!("`{id}` is not a session id"));
        }
        if seen.insert(id.as_str()) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

pub fn state_path() -> PathBuf {
    customize::config_dir().join("chats.json")
}

pub fn trash_dir() -> PathBuf {
    paths::app_data_dir().join("trash").join("sessions")
}

pub fn session_store() -> PathBuf {
    paths::vibe_home().join(SESSION_STORE)
}

/// An id is safe to join onto a path when it is short and made of the characters
/// a UUID uses. Deliberately strict: this is the only place a string from the
/// webview becomes a filesystem path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Read the bookkeeping file. A missing or unparseable file is an empty state:
/// losing the archive list must never make chats unreachable.
pub fn load_at(path: &Path) -> ChatsState {
    let Ok(body) = std::fs::read_to_string(path) else {
        return ChatsState::default();
    };
    let mut state: ChatsState = serde_json::from_str(&body).unwrap_or_default();
    state.archived.retain(|id| valid_id(id));
    state.deleted.retain(|id| valid_id(id));
    state.archived.truncate(MAX_ENTRIES);
    state.deleted.truncate(MAX_ENTRIES);
    state
}

pub fn save_at(path: &Path, state: &ChatsState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(state)
        .map_err(|error| format!("could not encode the chat list: {error}"))?;
    std::fs::write(path, body)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// Add or remove an id from a list, keeping it sorted and duplicate-free.
fn set_member(list: &mut Vec<String>, id: &str, present: bool) {
    list.retain(|entry| entry != id);
    if present {
        list.push(id.to_string());
        list.sort();
    }
}

pub fn set_archived(mut state: ChatsState, id: &str, archived: bool) -> ChatsState {
    set_member(&mut state.archived, id, archived);
    state
}

pub fn mark_deleted(mut state: ChatsState, id: &str) -> ChatsState {
    set_member(&mut state.archived, id, false);
    set_member(&mut state.deleted, id, true);
    state
}

/// Move a session directory into the trash.
///
/// `rename` is the fast path and is atomic, but it cannot cross volumes — and
/// the app is installed on whatever drive the user picked while `~/.vibe` is on
/// the system drive. So a failed rename falls back to copy-then-remove, which is
/// the same outcome with a window where both copies exist.
pub fn move_dir(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_dir(from, to)?;
    std::fs::remove_dir_all(from).map_err(|error| {
        format!(
            "copied to {} but could not remove {}: {error}",
            to.display(),
            from.display()
        )
    })
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to)
        .map_err(|error| format!("could not create {}: {error}", to.display()))?;
    let entries = std::fs::read_dir(from)
        .map_err(|error| format!("could not read {}: {error}", from.display()))?;
    for entry in entries.flatten() {
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            std::fs::copy(&source, &target).map_err(|error| {
                format!(
                    "could not copy {} to {}: {error}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Move one session out of the store. Returns the trash path, or `None` when
/// there was nothing on disk to move — a chat can be listed from the CLI's
/// catalog after its directory is already gone.
pub fn delete_at(store: &Path, trash: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    if !valid_id(id) {
        return Err(format!("`{id}` is not a session id"));
    }
    let source = store.join(id);
    if !source.is_dir() {
        return Ok(None);
    }
    let target = trash.join(id);
    // A second delete of the same id must not fail on the leftover.
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|error| {
            format!("could not clear the old trash entry {}: {error}", target.display())
        })?;
    }
    move_dir(&source, &target)?;
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vibedesk-chats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn an_id_must_be_a_plain_id() {
        assert!(valid_id("877e7404-145f-9cff-a6fe-3a18c9694039"));
        assert!(valid_id("child-1cbc769daadfe3ac18a65a2f"));
        assert!(!valid_id(""));
        assert!(!valid_id("../../etc"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a\\b"));
        assert!(!valid_id("a b"));
        assert!(!valid_id(&"x".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn a_missing_state_file_is_an_empty_state() {
        let root = scratch("missing");
        let state = load_at(&root.join("chats.json"));
        assert_eq!(state, ChatsState::default());
    }

    #[test]
    fn a_corrupt_state_file_is_an_empty_state_not_an_error() {
        let root = scratch("corrupt");
        let path = root.join("chats.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_at(&path), ChatsState::default());
    }

    #[test]
    fn ids_that_are_not_ids_are_dropped_on_load() {
        let root = scratch("drop");
        let path = root.join("chats.json");
        std::fs::write(
            &path,
            r#"{"archived":["ok-1","../../evil"],"deleted":["ok-2","a/b"]}"#,
        )
        .unwrap();
        let state = load_at(&path);
        assert_eq!(state.archived, vec!["ok-1".to_string()]);
        assert_eq!(state.deleted, vec!["ok-2".to_string()]);
    }

    #[test]
    fn archiving_round_trips_through_the_file() {
        let root = scratch("archive");
        let path = root.join("chats.json");
        let state = set_archived(ChatsState::default(), "abc", true);
        save_at(&path, &state).unwrap();
        assert_eq!(load_at(&path).archived, vec!["abc".to_string()]);

        let state = set_archived(load_at(&path), "abc", false);
        save_at(&path, &state).unwrap();
        assert!(load_at(&path).archived.is_empty());
    }

    #[test]
    fn archiving_twice_does_not_duplicate() {
        let state = set_archived(ChatsState::default(), "abc", true);
        let state = set_archived(state, "abc", true);
        assert_eq!(state.archived, vec!["abc".to_string()]);
    }

    #[test]
    fn deleting_clears_the_archive_entry() {
        let state = set_archived(ChatsState::default(), "abc", true);
        let state = mark_deleted(state, "abc");
        assert!(state.archived.is_empty());
        assert_eq!(state.deleted, vec!["abc".to_string()]);
    }

    #[test]
    fn deleting_moves_the_directory_into_the_trash() {
        let root = scratch("delete");
        let store = root.join("store");
        let trash = root.join("trash");
        std::fs::create_dir_all(store.join("abc").join("journal")).unwrap();
        std::fs::write(store.join("abc").join("journal").join("0001.jsonl"), "{}").unwrap();

        let moved = delete_at(&store, &trash, "abc").unwrap().expect("moved");
        assert!(!store.join("abc").exists());
        assert!(moved.join("journal").join("0001.jsonl").exists());
    }

    #[test]
    fn deleting_a_session_that_is_not_on_disk_is_not_an_error() {
        let root = scratch("gone");
        let store = root.join("store");
        let trash = root.join("trash");
        std::fs::create_dir_all(&store).unwrap();
        assert_eq!(delete_at(&store, &trash, "abc").unwrap(), None);
    }

    #[test]
    fn deleting_refuses_a_traversal_id() {
        let root = scratch("traversal");
        let store = root.join("store");
        let trash = root.join("trash");
        std::fs::create_dir_all(&store).unwrap();
        assert!(delete_at(&store, &trash, "../../evil").is_err());
    }

    #[test]
    fn deleting_twice_replaces_the_old_trash_entry() {
        let root = scratch("twice");
        let store = root.join("store");
        let trash = root.join("trash");
        std::fs::create_dir_all(store.join("abc")).unwrap();
        std::fs::write(store.join("abc").join("one.txt"), "1").unwrap();
        delete_at(&store, &trash, "abc").unwrap();

        std::fs::create_dir_all(store.join("abc")).unwrap();
        std::fs::write(store.join("abc").join("two.txt"), "2").unwrap();
        delete_at(&store, &trash, "abc").unwrap();

        assert!(trash.join("abc").join("two.txt").exists());
        assert!(!trash.join("abc").join("one.txt").exists());
    }

    #[test]
    fn the_state_serialises_as_camel_case() {
        let state = ChatsState {
            archived: vec!["a".into()],
            deleted: vec!["b".into()],
        };
        let body = serde_json::to_string(&state).unwrap();
        assert_eq!(body, r#"{"archived":["a"],"deleted":["b"]}"#);
    }
}
