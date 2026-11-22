//! The memory store: one Markdown file per memory, plus an index.
//!
//! Ported from dream-memory's `store/filesystem.ts`. The on-disk shape is kept
//! exactly, because it is the part a person reads and edits by hand:
//!
//! ```text
//! <data>/memory/
//!   MEMORY.md              the index, newest first
//!   .consolidate-lock      mtime = when the last pass ran
//!   .sessions.json         which sessions have been logged
//!   memories/<id>.md       one memory, YAML-ish frontmatter then the body
//! ```
//!
//! Two deliberate departures from upstream:
//!
//! * Ids are slugs of the memory's name, not UUIDs. The model has to name an id
//!   when it updates or prunes a memory, and `testing-stack` is a name it can
//!   copy correctly where `9f2c…` is not.
//! * The index is rebuilt from the files rather than maintained alongside them,
//!   so a hand-edited directory cannot leave the index lying.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::paths;

/// The four kinds of memory upstream distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// Who the user is, and how they like to work.
    User,
    /// A correction or a confirmation about how to approach work.
    Feedback,
    /// Ongoing work, decisions, blockers.
    Project,
    /// A pointer to something outside the repository.
    Reference,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::User => "user",
            MemoryType::Feedback => "feedback",
            MemoryType::Project => "project",
            MemoryType::Reference => "reference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user" => Some(MemoryType::User),
            "feedback" => Some(MemoryType::Feedback),
            "project" => Some(MemoryType::Project),
            "reference" => Some(MemoryType::Reference),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MemoryType::User => "User preference",
            MemoryType::Feedback => "Feedback",
            MemoryType::Project => "Project context",
            MemoryType::Reference => "Reference",
        }
    }
}

/// One memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub name: String,
    /// One line, used for relevance scoring. Never the content.
    pub description: String,
    pub content: String,
    #[serde(rename = "type")]
    pub kind: MemoryType,
    pub created_at: i64,
    pub updated_at: i64,
    pub source: Option<String>,
}

/// What relevance scoring sees: frontmatter only, never the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryHeader {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: MemoryType,
    pub updated_at: i64,
}

impl From<&Memory> for MemoryHeader {
    fn from(memory: &Memory) -> Self {
        Self {
            id: memory.id.clone(),
            name: memory.name.clone(),
            description: memory.description.clone(),
            kind: memory.kind,
            updated_at: memory.updated_at,
        }
    }
}

/// The store, rooted at a directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(paths::app_data_dir().join("memory"))
    }
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn memories_dir(&self) -> PathBuf {
        self.root.join("memories")
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(".consolidate-lock")
    }

    fn sessions_path(&self) -> PathBuf {
        self.root.join(".sessions.json")
    }

    fn memory_path(&self, id: &str) -> AppResult<PathBuf> {
        // The id reaches the filesystem, so it is validated rather than trusted.
        // Upstream's shape, kept: a leading alphanumeric, then alphanumerics,
        // hyphens and underscores.
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .next()
                .map(|first| first.is_ascii_alphanumeric())
                .unwrap_or(false)
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
        if !valid {
            return Err(AppError::message(format!(
                "{id:?} is not a usable memory id"
            )));
        }
        Ok(self.memories_dir().join(format!("{id}.md")))
    }

    pub fn get(&self, id: &str) -> Option<Memory> {
        let path = self.memory_path(id).ok()?;
        let raw = std::fs::read_to_string(path).ok()?;
        deserialize(&raw)
    }

    pub fn set(&self, memory: &Memory) -> AppResult<()> {
        std::fs::create_dir_all(self.memories_dir()).map_err(|error| {
            AppError::message(format!("could not create the memory directory: {error}"))
        })?;
        let path = self.memory_path(&memory.id)?;
        std::fs::write(&path, serialize(memory))
            .map_err(|error| AppError::message(format!("could not write {}: {error}", path.display())))?;
        self.rebuild_index()
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        if let Ok(path) = self.memory_path(id) {
            let _ = std::fs::remove_file(path);
        }
        self.rebuild_index()
    }

    /// Frontmatter only, newest first.
    pub fn list_headers(&self) -> Vec<MemoryHeader> {
        let mut headers: Vec<MemoryHeader> = self
            .list()
            .iter()
            .map(MemoryHeader::from)
            .collect();
        headers.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        headers
    }

    /// Every memory, newest first.
    pub fn list(&self) -> Vec<Memory> {
        let Ok(entries) = std::fs::read_dir(self.memories_dir()) else {
            return Vec::new();
        };
        let mut memories: Vec<Memory> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|extension| extension == "md")
                    .unwrap_or(false)
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|raw| deserialize(&raw))
            .collect();
        memories.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        memories
    }

    pub fn last_consolidated_at(&self) -> i64 {
        std::fs::metadata(self.lock_path())
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0)
    }

    pub fn set_last_consolidated_at(&self, at: i64) -> AppResult<()> {
        std::fs::create_dir_all(&self.root).map_err(|error| {
            AppError::message(format!("could not create the memory directory: {error}"))
        })?;
        let path = self.lock_path();
        std::fs::write(&path, std::process::id().to_string())
            .map_err(|error| AppError::message(format!("could not write the lock: {error}")))?;
        // The mtime *is* the timestamp, which is how upstream stores it.
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(at.max(0) as u64);
        let _ = filetime_set(&path, when);
        Ok(())
    }

    /// Record that a session happened, so the gate can count them.
    pub fn log_session(&self, session_id: &str) -> AppResult<()> {
        let mut log = self.sessions();
        if log.iter().any(|entry| entry.id == session_id) {
            return Ok(());
        }
        log.push(SessionEntry {
            id: session_id.to_string(),
            logged_at: chrono::Utc::now().timestamp_millis(),
        });
        if log.len() > 1000 {
            let excess = log.len() - 1000;
            log.drain(..excess);
        }
        std::fs::create_dir_all(&self.root).map_err(|error| {
            AppError::message(format!("could not create the memory directory: {error}"))
        })?;
        let body = serde_json::to_string_pretty(&SessionLog { sessions: log })
            .map_err(|error| AppError::message(format!("could not serialise the session log: {error}")))?;
        std::fs::write(self.sessions_path(), body)
            .map_err(|error| AppError::message(format!("could not write the session log: {error}")))
    }

    pub fn count_sessions_since(&self, since: i64) -> usize {
        self.sessions()
            .into_iter()
            .filter(|entry| entry.logged_at > since)
            .count()
    }

    fn sessions(&self) -> Vec<SessionEntry> {
        let Ok(raw) = std::fs::read_to_string(self.sessions_path()) else {
            return Vec::new();
        };
        serde_json::from_str::<SessionLog>(&raw)
            .map(|log| log.sessions)
            .unwrap_or_default()
    }

    pub fn clear(&self) -> AppResult<()> {
        let _ = std::fs::remove_dir_all(&self.root);
        Ok(())
    }

    /// Rewrite `MEMORY.md` from the files on disk.
    fn rebuild_index(&self) -> AppResult<()> {
        let headers = self.list_headers();
        let mut lines = vec![
            "# Memory Index".to_string(),
            String::new(),
            format!(
                "> Auto-generated by VibeDesk | {} memories | Last updated: {}",
                headers.len(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            ),
            String::new(),
            "## Memories".to_string(),
            String::new(),
        ];

        // Upstream caps the index at 200 lines and 25 KB so it stays cheap to
        // read. Both caps are kept.
        const MAX_LINES: usize = 200;
        const MAX_BYTES: usize = 25_000;
        for header in &headers {
            lines.push(format!(
                "- [{}](memories/{}.md) — {}",
                header.name, header.id, header.description
            ));
            if lines.len() >= MAX_LINES {
                lines.push(String::new());
                lines.push(format!("_...and {} more_", headers.len() - lines.len() + 6));
                break;
            }
        }

        let mut body = lines.join("\n");
        if body.len() > MAX_BYTES {
            body.truncate(MAX_BYTES);
            body.push_str("\n...");
        }
        std::fs::write(self.index_path(), body).map_err(|error| {
            AppError::message(format!("could not write the memory index: {error}"))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionEntry {
    id: String,
    logged_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionLog {
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

/// A slug that is also a legal id.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 96 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "memory".to_string()
    } else {
        trimmed
    }
}

/// An id that is not already taken.
pub fn free_id(store: &Store, name: &str) -> String {
    let base = slug(name);
    if store.get(&base).is_none() {
        return base;
    }
    for suffix in 2..1000 {
        let candidate = format!("{base}-{suffix}");
        if store.get(&candidate).is_none() {
            return candidate;
        }
    }
    format!("{base}-{}", chrono::Utc::now().timestamp_millis())
}

fn serialize(memory: &Memory) -> String {
    let mut lines = vec![
        "---".to_string(),
        format!("id: {}", escape(&memory.id)),
        format!("name: {}", escape(&memory.name)),
        format!("description: {}", escape(&memory.description)),
        format!("type: {}", memory.kind.as_str()),
        format!("createdAt: {}", memory.created_at),
        format!("updatedAt: {}", memory.updated_at),
    ];
    if let Some(source) = &memory.source {
        lines.push(format!("source: {}", escape(source)));
    }
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(memory.content.clone());
    lines.join("\n")
}

/// Quote a value when a bare one would not survive the round trip.
fn escape(value: &str) -> String {
    let collapsed = value.replace(['\r', '\n'], " ");
    let needs_quotes = collapsed.contains(':')
        || collapsed.starts_with('"')
        || collapsed.starts_with('\'')
        || collapsed.starts_with('#')
        || collapsed.starts_with('-');
    if needs_quotes {
        format!("\"{}\"", collapsed.replace('"', "\\\""))
    } else {
        collapsed
    }
}

fn deserialize(raw: &str) -> Option<Memory> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let content = rest[end + 4..].trim().to_string();

    let mut id = None;
    let mut name = None;
    let mut description = String::new();
    let mut kind = None;
    let mut created_at = 0i64;
    let mut updated_at = 0i64;
    let mut source = None;

    for line in frontmatter.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on the *first* colon only: descriptions and URLs contain colons.
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim();
        let mut value = line[colon + 1..].trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        match key {
            "id" => id = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "description" => description = value.to_string(),
            "type" => kind = MemoryType::parse(value),
            "createdAt" => created_at = value.parse().unwrap_or(0),
            "updatedAt" => updated_at = value.parse().unwrap_or(0),
            "source" => source = Some(value.to_string()),
            _ => {}
        }
    }

    Some(Memory {
        id: id?,
        name: name?,
        description,
        content,
        kind: kind?,
        created_at,
        updated_at,
        source,
    })
}

/// Set a file's mtime without pulling in a dependency for it.
fn filetime_set(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(when)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let root = std::env::temp_dir().join(format!(
            "vibedesk-memory-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        Store::new(root)
    }

    fn memory(id: &str, name: &str) -> Memory {
        Memory {
            id: id.into(),
            name: name.into(),
            description: "one line".into(),
            content: "the body".into(),
            kind: MemoryType::Project,
            created_at: 1,
            updated_at: 2,
            source: None,
        }
    }

    #[test]
    fn a_memory_round_trips_through_the_file() {
        let store = temp_store();
        store.set(&memory("testing-stack", "Testing stack")).unwrap();
        let read = store.get("testing-stack").unwrap();
        assert_eq!(read.name, "Testing stack");
        assert_eq!(read.content, "the body");
        assert_eq!(read.kind, MemoryType::Project);
        assert_eq!(read.updated_at, 2);
        store.clear().unwrap();
    }

    #[test]
    fn a_description_with_a_colon_survives() {
        let store = temp_store();
        let mut entry = memory("urls", "URLs");
        entry.description = "Docs live at https://example.test:8443/docs".into();
        store.set(&entry).unwrap();
        assert_eq!(
            store.get("urls").unwrap().description,
            "Docs live at https://example.test:8443/docs"
        );
        store.clear().unwrap();
    }

    #[test]
    fn the_index_lists_every_memory() {
        let store = temp_store();
        store.set(&memory("one", "First")).unwrap();
        store.set(&memory("two", "Second")).unwrap();
        let index = std::fs::read_to_string(store.index_path()).unwrap();
        assert!(index.contains("memories/one.md"));
        assert!(index.contains("memories/two.md"));
        assert!(index.contains("2 memories"));
        store.clear().unwrap();
    }

    #[test]
    fn deleting_a_memory_removes_it_from_the_index() {
        let store = temp_store();
        store.set(&memory("one", "First")).unwrap();
        store.delete("one").unwrap();
        let index = std::fs::read_to_string(store.index_path()).unwrap();
        assert!(!index.contains("memories/one.md"));
        assert!(store.get("one").is_none());
        store.clear().unwrap();
    }

    #[test]
    fn an_id_that_would_escape_the_directory_is_refused() {
        let store = temp_store();
        assert!(store.memory_path("../../etc/passwd").is_err());
        assert!(store.memory_path("").is_err());
        assert!(store.memory_path("ok-id_1").is_ok());
    }

    #[test]
    fn a_slug_is_lowercase_and_hyphenated() {
        assert_eq!(slug("Testing Stack"), "testing-stack");
        assert_eq!(slug("  weird   spacing!! "), "weird-spacing");
        assert_eq!(slug("!!!"), "memory");
    }

    #[test]
    fn a_taken_id_gets_a_suffix_rather_than_overwriting() {
        let store = temp_store();
        store.set(&memory("testing-stack", "Testing stack")).unwrap();
        assert_eq!(free_id(&store, "Testing stack"), "testing-stack-2");
        store.clear().unwrap();
    }

    #[test]
    fn a_session_is_logged_once_and_counted_since_a_time() {
        let store = temp_store();
        store.log_session("s1").unwrap();
        store.log_session("s1").unwrap();
        store.log_session("s2").unwrap();
        assert_eq!(store.count_sessions_since(0), 2);
        assert_eq!(store.count_sessions_since(i64::MAX), 0);
        store.clear().unwrap();
    }

    #[test]
    fn the_lock_records_when_the_last_pass_ran() {
        let store = temp_store();
        assert_eq!(store.last_consolidated_at(), 0);
        let when = chrono::Utc::now().timestamp_millis();
        store.set_last_consolidated_at(when).unwrap();
        let read = store.last_consolidated_at();
        assert!((read - when).abs() < 2000, "{read} vs {when}");
        store.clear().unwrap();
    }

    #[test]
    fn a_file_with_no_frontmatter_is_ignored_rather_than_guessed() {
        let store = temp_store();
        std::fs::create_dir_all(store.memories_dir()).unwrap();
        std::fs::write(store.memories_dir().join("junk.md"), "just text").unwrap();
        assert!(store.list().is_empty());
        store.clear().unwrap();
    }
}
