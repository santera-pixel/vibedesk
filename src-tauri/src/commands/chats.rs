use std::path::Path;

use tauri::State;

use crate::chats::{self, BulkDelete, BulkFailure, ChatsState};
use crate::error::{AppError, AppResult};
use crate::AppState;

#[tauri::command]
pub fn chats_state() -> ChatsState {
    chats::load_at(&chats::state_path())
}

#[tauri::command]
pub fn chats_archive(session_id: String, archived: bool) -> AppResult<ChatsState> {
    if !chats::valid_id(&session_id) {
        return Err(AppError::message(format!(
            "`{session_id}` is not a session id"
        )));
    }
    let path = chats::state_path();
    let state = chats::set_archived(chats::load_at(&path), &session_id, archived);
    chats::save_at(&path, &state).map_err(AppError::message)?;
    Ok(state)
}

/// Refuse to delete the chat VibeDesk has open.
///
/// The agent writes to one session at a time — the one on screen — so that is
/// the only session whose directory must not move. Every other chat is fair
/// game, including ones the agent loaded earlier in this run and still holds a
/// lock for.
fn refuse_if_open(session_id: &str, open: Option<&str>) -> AppResult<()> {
    if open == Some(session_id) {
        return Err(AppError::message(
            "That chat is open. Switch to another chat before deleting it.",
        ));
    }
    Ok(())
}

/// Move a chat out of the CLI's session store and into VibeDesk's trash.
///
/// Refused for the open chat: the agent is writing to it, and moving the
/// directory out from under it is how the app dies. The rail also disables the
/// item for the open chat, but the check belongs here too — the webview is not
/// the only thing that can call a command.
#[tauri::command]
pub async fn chats_delete(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<ChatsState> {
    if !chats::valid_id(&session_id) {
        return Err(AppError::message(format!(
            "`{session_id}` is not a session id"
        )));
    }
    let open = state.bridge.status().await.session_id;
    refuse_if_open(&session_id, open.as_deref())?;

    chats::delete_at(&chats::session_store(), &chats::trash_dir(), &session_id)
        .map_err(AppError::message)?;

    let path = chats::state_path();
    let next = chats::mark_deleted(chats::load_at(&path), &session_id);
    chats::save_at(&path, &next).map_err(AppError::message)?;
    Ok(next)
}

/// Archive or restore many chats in one write.
///
/// Archiving is a list edit, so nothing here can fail per id: the only failure
/// is the write itself, and that is reported once rather than per chat. That is
/// why `failed` is always empty here — a field that could never be filled would
/// be a lie in the response shape.
#[tauri::command]
pub fn chats_archive_many(ids: Vec<String>, archived: bool) -> AppResult<BulkDelete> {
    let ids = chats::bulk_ids(&ids).map_err(AppError::message)?;
    let path = chats::state_path();
    let state = archive_many_in(chats::load_at(&path), &ids, archived);
    chats::save_at(&path, &state).map_err(AppError::message)?;
    Ok(BulkDelete {
        state,
        done: ids,
        skipped: Vec::new(),
        failed: Vec::new(),
    })
}

/// Delete many chats, skipping the one the agent has open.
///
/// The open chat is skipped rather than refused. A bulk delete of ten chats
/// must not abort because one of them is on screen — the caller gets it back in
/// `skipped` and says so. Every other chat is fair game, including ones the
/// agent loaded earlier in this run and still holds a lock for.
#[tauri::command]
pub async fn chats_delete_many(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> AppResult<BulkDelete> {
    let ids = chats::bulk_ids(&ids).map_err(AppError::message)?;
    let open = state.bridge.status().await.session_id;
    let path = chats::state_path();
    let result = delete_many_at(
        &chats::session_store(),
        &chats::trash_dir(),
        chats::load_at(&path),
        &ids,
        open.as_deref(),
    );
    chats::save_at(&path, &result.state).map_err(AppError::message)?;
    Ok(result)
}

/// The archive loop, without the Tauri plumbing, so it can be tested.
fn archive_many_in(mut state: ChatsState, ids: &[String], archived: bool) -> ChatsState {
    for id in ids {
        state = chats::set_archived(state, id, archived);
    }
    state
}

/// The delete loop, without the Tauri plumbing, so it can be tested.
///
/// One chat that cannot move must not stop the rest, so every id lands in
/// exactly one of `done`, `skipped` or `failed`.
fn delete_many_at(
    store: &Path,
    trash: &Path,
    mut state: ChatsState,
    ids: &[String],
    open: Option<&str>,
) -> BulkDelete {
    let mut done = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for id in ids {
        if open == Some(id.as_str()) {
            skipped.push(id.clone());
            continue;
        }
        match chats::delete_at(store, trash, id) {
            // `Ok(None)` means the directory was already gone. The chat is
            // still gone, which is what was asked for.
            Ok(_) => {
                state = chats::mark_deleted(state, id);
                done.push(id.clone());
            }
            Err(error) => failed.push(BulkFailure {
                session_id: id.clone(),
                error,
            }),
        }
    }

    BulkDelete {
        state,
        done,
        skipped,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traversal_id_is_refused_before_it_reaches_a_path() {
        let error = chats_archive("../../evil".into(), true).unwrap_err();
        assert!(format!("{error:?}").contains("not a session id"));
    }

    #[test]
    fn only_the_open_chat_is_refused() {
        assert!(refuse_if_open("abc", Some("abc")).is_err());
        assert!(refuse_if_open("abc", Some("def")).is_ok());
        assert!(refuse_if_open("abc", None).is_ok());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "vibedesk-bulk-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn session(store: &Path, id: &str) {
        std::fs::create_dir_all(store.join(id)).unwrap();
        std::fs::write(store.join(id).join("session.json"), "{}").unwrap();
    }

    #[test]
    fn bulk_ids_drops_repeats_and_keeps_the_order_given() {
        let ids = chats::bulk_ids(&["b".into(), "a".into(), "b".into()]).unwrap();
        assert_eq!(ids, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn bulk_ids_refuses_a_traversal_before_it_reaches_a_path() {
        let error = chats::bulk_ids(&["ok".into(), "../../evil".into()]).unwrap_err();
        assert!(error.contains("not a session id"));
    }

    #[test]
    fn bulk_ids_refuses_more_than_the_cap() {
        let ids: Vec<String> = (0..chats::MAX_ENTRIES + 1).map(|n| format!("id{n}")).collect();
        let error = chats::bulk_ids(&ids).unwrap_err();
        assert!(error.contains("more than"));
    }

    #[test]
    fn archiving_many_applies_to_every_id_and_reverses_cleanly() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let state = archive_many_in(ChatsState::default(), &ids, true);
        assert_eq!(state.archived, vec!["a", "b", "c"]);

        let state = archive_many_in(state, &ids, false);
        assert!(state.archived.is_empty());
    }

    #[test]
    fn archiving_many_is_idempotent() {
        let ids = vec!["a".to_string()];
        let once = archive_many_in(ChatsState::default(), &ids, true);
        let twice = archive_many_in(once.clone(), &ids, true);
        assert_eq!(once, twice);
    }

    #[test]
    fn deleting_many_skips_the_open_chat_and_deletes_the_rest() {
        let root = scratch("skip-open");
        let store = root.join("store");
        let trash = root.join("trash");
        for id in ["a", "b", "c"] {
            session(&store, id);
        }

        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = delete_many_at(&store, &trash, ChatsState::default(), &ids, Some("b"));

        assert_eq!(result.done, vec!["a", "c"]);
        assert_eq!(result.skipped, vec!["b"]);
        assert!(result.failed.is_empty());
        assert_eq!(result.state.deleted, vec!["a", "c"]);
        assert!(store.join("b").is_dir(), "the open chat must not move");
        assert!(!store.join("a").exists());
        assert!(trash.join("a").is_dir());
    }

    #[test]
    fn one_chat_that_cannot_move_does_not_stop_the_others() {
        let root = scratch("partial");
        let store = root.join("store");
        let trash = root.join("trash");
        for id in ["a", "b"] {
            session(&store, id);
        }
        // A file where the trash entry should be makes the move fail.
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("a"), "not a directory").unwrap();

        let ids = vec!["a".to_string(), "b".to_string()];
        let result = delete_many_at(&store, &trash, ChatsState::default(), &ids, None);

        assert_eq!(result.done, vec!["b"]);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].session_id, "a");
        assert!(!result.failed[0].error.is_empty());
        assert_eq!(result.state.deleted, vec!["b"]);
    }

    #[test]
    fn deleting_a_chat_whose_directory_is_already_gone_still_counts_as_done() {
        let root = scratch("already-gone");
        let store = root.join("store");
        let trash = root.join("trash");
        std::fs::create_dir_all(&store).unwrap();

        let ids = vec!["ghost".to_string()];
        let result = delete_many_at(&store, &trash, ChatsState::default(), &ids, None);

        assert_eq!(result.done, vec!["ghost"]);
        assert!(result.failed.is_empty());
        assert_eq!(result.state.deleted, vec!["ghost"]);
    }
}
