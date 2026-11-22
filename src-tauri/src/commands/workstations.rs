use std::path::PathBuf;

use crate::error::{AppError, AppResult};
use crate::settings::{self, Workstation};

/// Workstations are just folders the user pinned. They live in VibeDesk's own
/// settings file, which is portable: an install on `D:` keeps its list on `D:`.
#[tauri::command]
pub fn workstations_list() -> Vec<Workstation> {
    settings::load().workstations
}

#[tauri::command]
pub fn workstation_add(path: String) -> AppResult<Vec<Workstation>> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(AppError::message(format!("{path} is not a folder")));
    }

    let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
    let value = crate::paths::strip_verbatim(&canonical.to_string_lossy());

    let mut current = settings::load();
    if current
        .workstations
        .iter()
        .any(|entry| entry.path.eq_ignore_ascii_case(&value))
    {
        return Ok(current.workstations);
    }

    let name = canonical
        .file_name()
        .map(|part| part.to_string_lossy().to_string())
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| value.clone());

    current.workstations.push(Workstation {
        path: value,
        name,
        added_at: chrono::Utc::now().to_rfc3339(),
    });
    settings::save(&current).map_err(AppError::message)?;
    Ok(current.workstations)
}

#[tauri::command]
pub fn workstation_remove(path: String) -> AppResult<Vec<Workstation>> {
    let mut current = settings::load();
    current
        .workstations
        .retain(|entry| !entry.path.eq_ignore_ascii_case(&path));
    settings::save(&current).map_err(AppError::message)?;
    Ok(current.workstations)
}

#[tauri::command]
pub fn workstation_rename(path: String, name: String) -> AppResult<Vec<Workstation>> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::message("a workstation needs a name"));
    }

    let mut current = settings::load();
    let Some(entry) = current
        .workstations
        .iter_mut()
        .find(|entry| entry.path.eq_ignore_ascii_case(&path))
    else {
        return Err(AppError::message("no such workstation"));
    };
    entry.name = trimmed.to_string();
    settings::save(&current).map_err(AppError::message)?;
    Ok(current.workstations)
}
