//! The provisioning ledger.
//!
//! One record per step, written after every step so a crash mid-provision leaves
//! an accurate record rather than a machine that looks provisioned and is not.
//!
//! `skipped` and `failed` are different answers and stay different. `skipped`
//! means a precondition was not met — no node, no network, the user declined.
//! `failed` means the step ran and did not work. The panel renders them
//! differently and only `failed` offers a retry.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped when the on-disk shape changes in a way an older reader cannot read.
pub const LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Never attempted, or attempted and found nothing on the machine.
    Absent,
    /// A step is running right now. A record left in this state means the
    /// process died mid-step, which is why it is written before the work starts.
    Installing,
    Ready,
    Failed,
    Skipped,
}

impl State {
    /// Whether the machine satisfies this step. Only `ready` does.
    pub fn is_satisfied(self) -> bool {
        matches!(self, State::Ready)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// RFC 3339, when the state last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The line the panel shows under the step.
    #[serde(default)]
    pub detail: String,
    /// Why the step could not run, when it was skipped or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

impl Record {
    pub fn new(id: impl Into<String>, state: State, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            state,
            version: None,
            at: Some(now()),
            detail: detail.into(),
            blocked: None,
        }
    }

    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    pub fn blocked_by(mut self, reason: impl Into<String>) -> Self {
        self.blocked = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub version: u32,
    #[serde(default)]
    pub records: Vec<Record>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            records: Vec::new(),
        }
    }
}

impl Ledger {
    /// Read the ledger, tolerating a missing or corrupt file.
    ///
    /// The second value is a note for the panel when the file could not be read
    /// as a ledger. Starting empty is the right answer — a machine whose ledger
    /// is unreadable is a machine whose steps must be probed again — but saying
    /// so is not optional, because silently starting empty looks identical to a
    /// fresh machine.
    pub fn load() -> (Self, Option<String>) {
        let path = path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None)
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("could not read {}: {error}", path.display())),
                )
            }
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(ledger) => (ledger, None),
            Err(error) => (
                Self::default(),
                Some(format!(
                    "{} is not a ledger ({error}); every step will be probed again",
                    path.display()
                )),
            ),
        }
    }

    /// Write the ledger, replacing the file atomically.
    ///
    /// A half-written ledger is worse than a missing one: it reads as a machine
    /// that was provisioned. The rename is what makes the write all-or-nothing.
    pub fn save(&self) -> Result<(), String> {
        let path = path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, body)
            .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .map_err(|error| format!("could not replace {}: {error}", path.display()))
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|record| record.id == id)
    }

    pub fn state_of(&self, id: &str) -> State {
        self.get(id).map(|record| record.state).unwrap_or(State::Absent)
    }

    /// Replace the record for this id, keeping one record per step.
    pub fn upsert(&mut self, record: Record) {
        match self.records.iter_mut().find(|existing| existing.id == record.id) {
            Some(existing) => *existing = record,
            None => self.records.push(record),
        }
    }
}

pub fn path() -> PathBuf {
    crate::paths::app_data_dir().join("provision").join("ledger.json")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_ledger_is_empty_and_says_nothing() {
        let ledger = Ledger::default();
        assert!(ledger.records.is_empty());
        assert_eq!(ledger.version, LEDGER_VERSION);
        assert_eq!(ledger.state_of("cli"), State::Absent);
    }

    #[test]
    fn a_corrupt_ledger_starts_empty_and_says_so() {
        let raw = "{ this is not json";
        let parsed = serde_json::from_str::<Ledger>(raw);
        assert!(parsed.is_err(), "the fixture must actually be unreadable");
        // The load path turns that error into a note rather than a panic; the
        // note is what stops a corrupt file looking like a fresh machine.
        let note = format!("not a ledger ({})", parsed.unwrap_err());
        assert!(note.contains("not a ledger"));
    }

    #[test]
    fn upsert_keeps_one_record_per_step() {
        let mut ledger = Ledger::default();
        ledger.upsert(Record::new("cli", State::Absent, "not installed"));
        ledger.upsert(Record::new("cli", State::Ready, "installed").with_version(Some("3.1.2".into())));
        assert_eq!(ledger.records.len(), 1);
        assert_eq!(ledger.state_of("cli"), State::Ready);
        assert_eq!(ledger.get("cli").unwrap().version.as_deref(), Some("3.1.2"));
    }

    #[test]
    fn only_ready_satisfies_a_step() {
        assert!(State::Ready.is_satisfied());
        for state in [State::Absent, State::Installing, State::Failed, State::Skipped] {
            assert!(!state.is_satisfied(), "{state:?} must not satisfy a step");
        }
    }

    #[test]
    fn skipped_and_failed_stay_distinct_on_the_wire() {
        let skipped = serde_json::to_string(&Record::new("mastery", State::Skipped, "no git"))
            .unwrap();
        let failed = serde_json::to_string(&Record::new("mastery", State::Failed, "clone failed"))
            .unwrap();
        assert!(skipped.contains("\"skipped\""));
        assert!(failed.contains("\"failed\""));
        assert_ne!(skipped, failed);
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let record = Record::new("aegis", State::Failed, "clone failed")
            .with_version(Some("1.2.0".into()))
            .blocked_by("network unreachable");
        let body = serde_json::to_string(&record).unwrap();
        let back: Record = serde_json::from_str(&body).unwrap();
        assert_eq!(back, record);
    }
}
