//! The board: the append-only record of what the members said to each other.
//!
//! The board is the run's shared state, and it is deliberately not a router.
//! Every member reads it and writes to it directly; the engine only decides
//! *when* a member may run. A hub that digested each wave would put the lead on
//! the critical path of every message, which is the bottleneck a swarm exists to
//! remove.
//!
//! Two things live here that are easy to get wrong:
//!
//! **Arrival order is the order.** `seq` is assigned by the engine as entries
//! land, not by the member that wrote them, so two members writing in the same
//! millisecond still have a total order.
//!
//! **The dependency handoff is a digest, not a transcript.** A task that depends
//! on another gets the *decisions* that task made, not its conversation. That is
//! a deliberate departure from "pass the full trace": a full trace is the right
//! call for a single agent that must not lose context, and the wrong call for
//! parallel workers whose whole advantage is that their contexts are small.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::now;

/// What kind of thing was said.
///
/// The distinction that matters is `Decision` versus everything else: only
/// decisions cross a dependency edge. Evidence and claims stay on the board for
/// the user to read and never enter another member's brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoardKind {
    /// "I am taking t1." Lets the user see who is on what without asking.
    Claim,
    /// "I chose X because Y." The only kind that crosses a dependency edge.
    Decision,
    /// "This command produced this line." Backs a verdict.
    Evidence,
    /// A verifier's ruling.
    Verdict,
    /// A merge that could not be applied.
    Conflict,
    /// A member asking for a human decision.
    Escalation,
    /// Anything else worth recording.
    Note,
}

impl BoardKind {
    pub fn label(self) -> &'static str {
        match self {
            BoardKind::Claim => "claim",
            BoardKind::Decision => "decision",
            BoardKind::Evidence => "evidence",
            BoardKind::Verdict => "verdict",
            BoardKind::Conflict => "conflict",
            BoardKind::Escalation => "escalation",
            BoardKind::Note => "note",
        }
    }

    /// Whether this kind crosses a dependency edge.
    pub fn crosses_edges(self) -> bool {
        matches!(self, BoardKind::Decision)
    }
}

/// One entry on the board.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardEntry {
    /// Assigned by the engine, monotonic, never reused.
    pub seq: u64,
    pub at: String,
    /// The member that wrote it.
    pub from: String,
    /// Who it is for. Empty means everyone.
    #[serde(default)]
    pub to: Vec<String>,
    pub kind: BoardKind,
    pub text: String,
    /// The task it concerns, when it concerns one.
    #[serde(default)]
    pub task: Option<String>,
    /// Commands and their output lines, for `Evidence` and `Verdict`.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The one command a verdict names as its failing-first test.
    ///
    /// Carried on the entry rather than parsed out of `evidence`, because the
    /// engine runs it and a command it has to guess at is a command it cannot
    /// run.
    #[serde(default)]
    pub command: Option<String>,
    /// The ruling a verdict commits to, when it commits to one.
    ///
    /// A boolean the verifier has to write is harder to make ambiguous than a
    /// sentence the engine reads the first word of: the first live run marked a
    /// task verified on a verdict that opened with "FAILED". `None` is an entry
    /// that omitted the field — an older entry, or a verifier that wrote only
    /// prose — and the first-word parse is the fallback for it.
    #[serde(default)]
    pub passed: Option<bool>,
    /// A verdict that asks the lead for a decision instead of a retry.
    ///
    /// A verifier can find a change that is correct and still cannot be finished
    /// by the task that made it — a caller in a file the task does not own. The
    /// answer is an amendment, not another attempt, and the verifier used to have
    /// to write a *second* file to say so. The sixth live run's v-t1 wrote
    /// "Escalating rather than failing, per the correct-change-missing-permission
    /// rule. See escalation.json" into its verdict and never wrote the file, so
    /// the run retried a task the verifier had already declared unfixable. One
    /// entry, one field: the escalation travels with the ruling it belongs to.
    #[serde(default)]
    pub escalate: Option<String>,
}

impl BoardEntry {
    pub fn new(from: impl Into<String>, kind: BoardKind, text: impl Into<String>) -> Self {
        Self {
            seq: 0,
            at: now(),
            from: from.into(),
            to: Vec::new(),
            kind,
            text: text.into(),
            task: None,
            evidence: Vec::new(),
            command: None,
            passed: None,
            escalate: None,
        }
    }

    pub fn to(mut self, members: &[&str]) -> Self {
        self.to = members.iter().map(|m| m.to_string()).collect();
        self
    }

    pub fn about(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }

    /// Whether this entry is addressed to a member, or to everyone.
    pub fn reaches(&self, member: &str) -> bool {
        self.to.is_empty() || self.to.iter().any(|m| m == member)
    }

    /// Whether this entry is a broadcast.
    pub fn is_broadcast(&self) -> bool {
        self.to.is_empty()
    }

    /// Whether this entry is a run-level broadcast rather than a task's own.
    ///
    /// A decision scoped to a task is not a broadcast even when it names no
    /// recipient: it belongs to that task's edge, and copying it into every
    /// worker's digest would turn one task's choice into everyone's constraint.
    /// `"run"` is the scope the lead uses for a decision that really is global.
    pub fn is_global(&self) -> bool {
        self.to.is_empty() && matches!(self.task.as_deref(), None | Some("run"))
    }
}

/// How many decisions a digest carries, and how long it may be.
///
/// A digest that grows without bound is a transcript with extra steps. Twelve
/// decisions and 2400 characters is enough to carry the choices that constrain
/// the next task and small enough that a worker's brief stays a brief.
pub const DIGEST_MAX_ENTRIES: usize = 12;
pub const DIGEST_MAX_CHARS: usize = 2400;

/// The board, in memory, with an append-only file behind it.
#[derive(Debug, Clone, Default)]
pub struct Board {
    entries: Vec<BoardEntry>,
    next_seq: u64,
}

/// Whether an entry belongs to the task at hand rather than to the cross-task
/// chatter a digest is capped against.
///
/// Pinned: anything the board recorded against this task, and every escalation,
/// conflict or verdict anywhere in the run. Everything else — a dependency's
/// decision, the lead's broadcast constraint — is context, and context is capped
/// so a brief stays a brief.
fn constrains_the_work(entry: &BoardEntry, task: &str) -> bool {
    if entry.task.as_deref() == Some(task) {
        return true;
    }
    matches!(
        entry.kind,
        BoardKind::Escalation | BoardKind::Conflict | BoardKind::Verdict
    )
}

/// One line of a digest.
fn digest_line(entry: &BoardEntry) -> String {
    let scope = entry.task.as_deref().unwrap_or("run");
    format!("- [{scope}] {}\n", entry.text.trim())
}

impl Board {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_seq: 1,
        }
    }

    /// Append an entry, assigning its sequence number and timestamp.
    ///
    /// The timestamp is overwritten rather than trusted: a member's clock is
    /// not the run's clock, and the board's order is the run's order.
    pub fn append(&mut self, mut entry: BoardEntry) -> BoardEntry {
        entry.seq = self.next_seq;
        entry.at = now();
        self.next_seq += 1;
        self.entries.push(entry.clone());
        entry
    }

    /// Take an entry that was written to `board.jsonl` by something other than
    /// `append`, giving it a sequence number of its own.
    ///
    /// The timestamp is kept rather than overwritten: it is the moment the
    /// member wrote the entry, and that is the fact the board is recording. The
    /// sequence number is not kept, because a writer outside the engine has no
    /// way to know what the next one is — the twelfth live run's worker guessed
    /// 17 and the engine had already used 17, so the file carried two entries
    /// with the same number and the panel's order stopped meaning anything.
    pub fn adopt(&mut self, mut entry: BoardEntry) -> BoardEntry {
        entry.seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(entry.clone());
        entry
    }

    pub fn entries(&self) -> &[BoardEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last `n` entries, oldest first.
    pub fn tail(&self, n: usize) -> &[BoardEntry] {
        let start = self.entries.len().saturating_sub(n);
        &self.entries[start..]
    }

    /// Everything addressed to one member, oldest first.
    pub fn addressed_to(&self, member: &str) -> Vec<&BoardEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.reaches(member))
            .collect()
    }

    /// Everything one member wrote.
    pub fn by(&self, member: &str) -> Vec<&BoardEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.from == member)
            .collect()
    }

    /// The decisions that constrain a task, rendered as the brief it receives.
    ///
    /// Three sources, in this order: the decisions of the tasks it depends on,
    /// the decisions addressed to it, and the lead's broadcast decisions. Nothing
    /// else crosses. Returns an empty string when there is nothing to say, so the
    /// caller can omit the section entirely rather than print a heading over
    /// nothing.
    pub fn digest_for(&self, task: &str, deps: &[String]) -> String {
        let dep_set: BTreeSet<&str> = deps.iter().map(|d| d.as_str()).collect();

        let mut chosen: Vec<&BoardEntry> = Vec::new();
        let mut seen: BTreeSet<u64> = BTreeSet::new();

        // 1. Decisions made by the tasks this one waits for.
        for entry in &self.entries {
            if !entry.kind.crosses_edges() {
                continue;
            }
            let from_dep = entry
                .task
                .as_deref()
                .map(|t| dep_set.contains(t))
                .unwrap_or(false);
            if from_dep && seen.insert(entry.seq) {
                chosen.push(entry);
            }
        }

        // 2. Decisions addressed to this task specifically.
        for entry in &self.entries {
            if !entry.kind.crosses_edges() || entry.is_broadcast() {
                continue;
            }
            if entry.to.iter().any(|m| m == task) && seen.insert(entry.seq) {
                chosen.push(entry);
            }
        }

        // 3. Broadcast decisions — the lead's global constraints.
        for entry in &self.entries {
            if !entry.kind.crosses_edges() || !entry.is_global() {
                continue;
            }
            if seen.insert(entry.seq) {
                chosen.push(entry);
            }
        }

        // 4. The task's own record, and every escalation, conflict or verdict on
        // the board, whatever its scope. These are not chatter: this is the
        // history that would have told the audited run's w-t2 it had already been
        // refused once for writing outside its grant.
        for entry in &self.entries {
            if matches!(
                entry.kind,
                BoardKind::Escalation | BoardKind::Conflict | BoardKind::Verdict
            ) && seen.insert(entry.seq)
            {
                chosen.push(entry);
            }
        }
        for entry in &self.entries {
            if entry.task.as_deref() == Some(task) && seen.insert(entry.seq) {
                chosen.push(entry);
            }
        }

        if chosen.is_empty() {
            return String::new();
        }

        // Pinned entries are rendered in full; the rest is capped from the front,
        // because the most recent cross-task decisions are the ones that still
        // constrain the work. A cap on the entries that exist to stop a repeat
        // would be a cap on the memory the retry needed.
        let (pinned, chatter): (Vec<&BoardEntry>, Vec<&BoardEntry>) = chosen
            .into_iter()
            .partition(|entry| constrains_the_work(entry, task));

        let mut omitted = false;
        let mut kept = if chatter.len() > DIGEST_MAX_ENTRIES {
            omitted = true;
            chatter[chatter.len() - DIGEST_MAX_ENTRIES..].to_vec()
        } else {
            chatter
        };

        let mut width = 0usize;
        let mut retained: Vec<&BoardEntry> = Vec::new();
        while let Some(entry) = kept.pop() {
            let line = digest_line(entry);
            if width + line.len() > DIGEST_MAX_CHARS {
                omitted = true;
                break;
            }
            width += line.len();
            retained.push(entry);
        }
        retained.reverse();

        let mut ordered = pinned;
        ordered.sort_by_key(|entry| entry.seq);
        let mut out = String::new();
        for entry in ordered {
            out.push_str(&digest_line(entry));
        }
        if omitted {
            out.push_str("- …earlier decisions omitted\n");
        }
        for entry in retained {
            out.push_str(&digest_line(entry));
        }
        out
    }

    /// A stable hash of a digest, for idempotency.
    ///
    /// The engine writes a digest into a worker's brief once. If the same task
    /// is retried, the digest is recomputed and compared: an unchanged hash
    /// means the retry is not a new attempt at a changed problem.
    pub fn digest_hash(text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let digest = hasher.finalize();
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /* -------------------------------------------------------------- *
     * Persistence
     * -------------------------------------------------------------- */

    /// Read a board from `board.jsonl`.
    ///
    /// A line that does not parse is skipped rather than failing the load: a
    /// truncated final line is what a crash mid-append looks like, and losing
    /// the whole board to it would lose the run's record.
    pub fn load_at(dir: &Path) -> Self {
        let path = board_path(dir);
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Self::new();
        };
        let mut board = Self::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<BoardEntry>(line) {
                board.next_seq = board.next_seq.max(entry.seq + 1);
                board.entries.push(entry);
            }
        }
        board
    }

    /// Append one entry to `board.jsonl`.
    pub fn append_at(dir: &Path, entry: &BoardEntry) -> Result<(), String> {
        use std::io::Write;
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
        let path = board_path(dir);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        let line = serde_json::to_string(entry)
            .map_err(|error| format!("could not encode a board entry: {error}"))?;
        writeln!(file, "{line}").map_err(|error| format!("could not append to the board: {error}"))
    }
}

pub fn board_path(dir: &Path) -> PathBuf {
    dir.join("board.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(from: &str, task: &str, text: &str) -> BoardEntry {
        BoardEntry::new(from, BoardKind::Decision, text).about(task)
    }

    /// A verdict can carry the escalation it asks for, in the same entry.
    ///
    /// The sixth live run's v-t1 wrote "Escalating rather than failing ... See
    /// escalation.json" into its verdict and never wrote the file, so the engine
    /// retried a task the verifier had already declared unfixable. The field is
    /// on the entry now, and an entry without it is a plain ruling.
    #[test]
    fn a_verdict_carries_its_escalation_in_the_same_entry() {
        let body = r#"{
            "seq": 1,
            "at": "2026-11-19T04:30:00.000Z",
            "from": "v-t1",
            "kind": "verdict",
            "task": "t1",
            "text": "passed. The criterion fails on base and passes on the change.",
            "command": "node --test test/step.test.js",
            "evidence": ["node --test test/step.test.js -> pass 8"],
            "escalate": "src/main.js:112 still calls advanceGateField and this task does not own it"
        }"#;
        let entry: BoardEntry = serde_json::from_str(body).unwrap();
        assert_eq!(entry.kind, BoardKind::Verdict);
        assert_eq!(
            entry.escalate.as_deref(),
            Some("src/main.js:112 still calls advanceGateField and this task does not own it")
        );

        let plain: BoardEntry = serde_json::from_str(
            r#"{"seq":2,"at":"2026-11-19T04:30:00.000Z","from":"v-t1","kind":"verdict","task":"t1","text":"passed.","evidence":[]}"#,
        )
        .unwrap();
        assert_eq!(plain.escalate, None, "a plain ruling asks for nothing");
    }

    /// The ruling travels as a boolean, and an entry without one is not a
    /// malformed entry — it is an entry the first-word parse still reads.
    ///
    /// `board.jsonl` is read back on every tick, so a field the writer omits has
    /// to deserialize rather than fail the whole line.
    #[test]
    fn a_verdict_carries_its_ruling_as_a_boolean() {
        let body = r#"{
            "seq": 1,
            "at": "2026-11-19T04:30:00.000Z",
            "from": "v-t1",
            "kind": "verdict",
            "task": "t1",
            "text": "passed. The criterion fails on base and passes on the change.",
            "passed": false,
            "command": "node --test test/step.test.js",
            "evidence": ["node --test test/step.test.js -> pass 8"]
        }"#;
        let entry: BoardEntry = serde_json::from_str(body).unwrap();
        assert_eq!(
            entry.passed,
            Some(false),
            "the boolean is the ruling, whatever the text opens with"
        );

        let plain: BoardEntry = serde_json::from_str(
            r#"{"seq":2,"at":"2026-11-19T04:30:00.000Z","from":"v-t1","kind":"verdict","task":"t1","text":"passed.","evidence":[]}"#,
        )
        .unwrap();
        assert_eq!(
            plain.passed, None,
            "an entry that omits the field is read by the first word"
        );

        // And it survives the round trip through the file the panel reads.
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"passed\":false"), "{json}");
    }

    /// An entry written straight to the file keeps its time and gets a number
    /// the board can order.
    #[test]
    fn an_adopted_entry_keeps_its_time_and_takes_a_fresh_sequence_number() {
        let mut board = Board::new();
        board.append(decision("w-t1", "t1", "first"));

        let mut foreign = decision("w-t2", "t2", "written by hand");
        foreign.seq = 17;
        foreign.at = "2026-11-19T01:03:41.577Z".into();
        let adopted = board.adopt(foreign);

        assert_eq!(adopted.seq, 2, "the board assigns the number, not the writer");
        assert_eq!(
            adopted.at, "2026-11-19T01:03:41.577Z",
            "the moment the member wrote it is the fact being recorded"
        );
        assert_eq!(board.len(), 2);
        assert_eq!(board.entries()[1].seq, 2);
    }

    #[test]
    fn sequence_numbers_are_assigned_by_the_board_not_the_writer() {
        let mut board = Board::new();
        let a = board.append(BoardEntry::new("w1", BoardKind::Note, "first"));
        let b = board.append(BoardEntry::new("w2", BoardKind::Note, "second"));
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(board.len(), 2);
    }

    #[test]
    fn the_board_overwrites_a_writers_clock_with_its_own() {
        let mut board = Board::new();
        let mut entry = BoardEntry::new("w1", BoardKind::Note, "x");
        entry.at = "1999-01-01T00:00:00.000Z".into();
        let stored = board.append(entry);
        assert_ne!(stored.at, "1999-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_broadcast_reaches_everyone_and_an_addressed_entry_does_not() {
        let mut board = Board::new();
        board.append(BoardEntry::new("lead", BoardKind::Note, "all hands"));
        board.append(BoardEntry::new("lead", BoardKind::Note, "for w1").to(&["w1"]));

        assert_eq!(board.addressed_to("w1").len(), 2);
        assert_eq!(board.addressed_to("w2").len(), 1);
        assert!(board.entries()[0].is_broadcast());
        assert!(!board.entries()[1].is_broadcast());
    }

    #[test]
    fn a_digest_carries_the_decisions_of_the_tasks_it_depends_on() {
        let mut board = Board::new();
        board.append(decision("w1", "t1", "used a token bucket, not a sliding window"));
        board.append(decision("w2", "t2", "unrelated choice"));

        let digest = board.digest_for("t3", &["t1".to_string()]);
        assert!(digest.contains("token bucket"), "{digest}");
        assert!(!digest.contains("unrelated choice"), "{digest}");
    }

    #[test]
    fn a_digest_carries_broadcast_decisions() {
        let mut board = Board::new();
        board.append(decision("lead", "run", "no new dependencies"));
        let digest = board.digest_for("t1", &[]);
        assert!(digest.contains("no new dependencies"), "{digest}");
    }

    #[test]
    fn a_digest_carries_decisions_addressed_to_the_task() {
        let mut board = Board::new();
        board.append(
            BoardEntry::new("lead", BoardKind::Decision, "t2 owns the migration")
                .to(&["t2"]),
        );
        let digest = board.digest_for("t2", &[]);
        assert!(digest.contains("owns the migration"), "{digest}");
    }

    #[test]
    fn evidence_and_claims_never_cross_a_dependency_edge() {
        let mut board = Board::new();
        board.append(BoardEntry::new("w1", BoardKind::Evidence, "cargo test → ok").about("t1"));
        board.append(BoardEntry::new("w1", BoardKind::Claim, "taking t1").about("t1"));
        board.append(BoardEntry::new("w1", BoardKind::Note, "a note").about("t1"));

        let digest = board.digest_for("t2", &["t1".to_string()]);
        assert!(digest.is_empty(), "only decisions cross: {digest}");
    }

    #[test]
    fn an_empty_digest_is_empty_rather_than_a_heading_over_nothing() {
        let board = Board::new();
        assert_eq!(board.digest_for("t1", &[]), "");
    }

    #[test]
    fn a_digest_is_capped_and_says_so() {
        let mut board = Board::new();
        for index in 0..(DIGEST_MAX_ENTRIES + 6) {
            board.append(decision("w1", "t1", &format!("decision number {index}")));
        }
        let digest = board.digest_for("t2", &["t1".to_string()]);
        let lines = digest.lines().count();
        assert!(lines <= DIGEST_MAX_ENTRIES + 1, "{lines} lines");
        // The newest survive, the oldest are dropped.
        assert!(digest.contains(&format!("decision number {}", DIGEST_MAX_ENTRIES + 5)));
        assert!(!digest.contains("decision number 0\n"));
    }

    #[test]
    fn a_digest_is_byte_capped_even_when_the_entry_count_is_small() {
        let mut board = Board::new();
        for _ in 0..4 {
            board.append(decision("w1", "t1", &"x".repeat(1200)));
        }
        let digest = board.digest_for("t2", &["t1".to_string()]);
        assert!(digest.len() <= DIGEST_MAX_CHARS + 64, "{}", digest.len());
        assert!(digest.contains("omitted"));
    }

    /// The audited run's fifth fault: the digest a worker received was capped at
    /// twelve entries, so the newest cross-task chatter evicted the escalation the
    /// engine had raised against that very task. w-t2 then built the wrong thing a
    /// second time. What concerns a task is not chatter.
    #[test]
    fn a_task_sees_its_own_escalation_past_the_chatter_cap() {
        let mut board = Board::new();
        board.append(
            BoardEntry::new(
                "engine",
                BoardKind::Escalation,
                "w-t2 committed files outside its grant",
            )
            .about("t2"),
        );
        for index in 0..(DIGEST_MAX_ENTRIES + 6) {
            board.append(decision("w1", "t1", &format!("decision number {index}")));
        }

        let digest = board.digest_for("t2", &["t1".to_string()]);
        assert!(digest.contains("outside its grant"), "{digest}");
        // The chatter is still capped, and still keeps the newest.
        let chatter = digest
            .lines()
            .filter(|line| line.contains("decision number"))
            .count();
        assert_eq!(chatter, DIGEST_MAX_ENTRIES, "{digest}");
        assert!(digest.contains("decision number 17"), "{digest}");
        assert!(!digest.contains("decision number 5"), "{digest}");
    }

    /// A task's own record is never truncated, however many entries it holds.
    ///
    /// The cap exists so another task's context does not crowd out this one's
    /// work. Applied to this one's own history it does the opposite: it deletes
    /// the record of what was already tried.
    #[test]
    fn a_tasks_own_record_is_never_truncated() {
        let mut board = Board::new();
        let attempts = DIGEST_MAX_ENTRIES * 3;
        for index in 0..attempts {
            board.append(
                BoardEntry::new("w-t2", BoardKind::Note, format!("attempt {index}")).about("t2"),
            );
        }

        let digest = board.digest_for("t2", &[]);
        for index in 0..attempts {
            assert!(
                digest.contains(&format!("attempt {index}\n")),
                "lost attempt {index} of {attempts}"
            );
        }
    }

    #[test]
    fn a_digest_hash_is_stable_and_short() {
        let a = Board::digest_hash("hello");
        let b = Board::digest_hash("hello");
        let c = Board::digest_hash("hello!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn a_board_round_trips_through_jsonl() {
        let dir = std::env::temp_dir().join(format!("vd-board-{}", super::super::new_run_id()));
        let mut board = Board::new();
        board.append(decision("w1", "t1", "chose a"));
        board.append(BoardEntry::new("v1", BoardKind::Verdict, "passed").about("t1"));
        for entry in board.entries() {
            Board::append_at(&dir, entry).unwrap();
        }

        let back = Board::load_at(&dir);
        assert_eq!(back.len(), 2);
        assert_eq!(back.entries()[0].text, "chose a");
        assert_eq!(back.entries()[1].kind, BoardKind::Verdict);
        // The next append continues the sequence rather than restarting it.
        let mut back = back;
        let next = back.append(BoardEntry::new("w2", BoardKind::Note, "after"));
        assert_eq!(next.seq, 3);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_truncated_final_line_does_not_lose_the_board() {
        let dir = std::env::temp_dir().join(format!("vd-board-trunc-{}", super::super::new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut board = Board::new();
        board.append(decision("w1", "t1", "kept"));
        Board::append_at(&dir, &board.entries()[0]).unwrap();
        // Simulate a crash mid-append.
        let path = board_path(&dir);
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("{\"seq\":2,\"at\":\"2024");
        std::fs::write(&path, body).unwrap();

        let back = Board::load_at(&dir);
        assert_eq!(back.len(), 1);
        assert_eq!(back.entries()[0].text, "kept");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_board_file_is_an_empty_board_not_an_error() {
        let dir = std::env::temp_dir().join(format!("vd-board-none-{}", super::super::new_run_id()));
        let board = Board::load_at(&dir);
        assert!(board.is_empty());
    }

    #[test]
    fn the_tail_returns_the_newest_entries_oldest_first() {
        let mut board = Board::new();
        for index in 0..10 {
            board.append(BoardEntry::new("w1", BoardKind::Note, format!("n{index}")));
        }
        let tail = board.tail(3);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].text, "n7");
        assert_eq!(tail[2].text, "n9");
        // Asking for more than there is returns everything.
        assert_eq!(board.tail(100).len(), 10);
    }

    #[test]
    fn entries_by_one_member_are_filterable() {
        let mut board = Board::new();
        board.append(BoardEntry::new("w1", BoardKind::Note, "a"));
        board.append(BoardEntry::new("w2", BoardKind::Note, "b"));
        board.append(BoardEntry::new("w1", BoardKind::Note, "c"));
        assert_eq!(board.by("w1").len(), 2);
        assert_eq!(board.by("w2").len(), 1);
        assert_eq!(board.by("nobody").len(), 0);
    }
}
