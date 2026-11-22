//! Post-run reflection: what a future run should know about this repository.
//!
//! The repo already owns the machinery — `capabilities::memory` runs
//! Orient → Gather → Consolidate → Prune over finished sessions and writes
//! memories the proxy injects into the transient system prompt. This is a
//! specialisation, not a second memory system: it produces the same kind of
//! record, with one extra rule.
//!
//! **A gotcha with no source is not written.** Every gotcha must name the
//! verdict, conflict or escalation that proves it. A reflection pass that
//! free-associates about a repository produces plausible advice that the next
//! run then trusts, which is worse than no memory at all — the failure is
//! invisible and compounding.

use serde::{Deserialize, Serialize};

use super::board::{Board, BoardEntry, BoardKind};
use super::{Gotcha, Reflection, now};

/// What the reflection pass writes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionDraft {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub gotchas: Vec<GotchaDraft>,
    /// Methods this run had to invent, written as skills the machine lacks.
    ///
    /// The other half of the same idea as a gotcha. A gotcha is a fact about the
    /// repository the next run should not rediscover; a skill is a *method* the
    /// next run should not reinvent. Both are written from what the run actually
    /// did, and neither is applied without a human: a proposal lands in the run's
    /// own directory, and accepting it is a click.
    #[serde(default)]
    pub skills: Vec<SkillDraft>,
}

/// A skill the run decided this machine is missing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDraft {
    /// Lowercase, hyphenated. Becomes the directory name.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The `SKILL.md` body, without frontmatter.
    #[serde(default)]
    pub body: String,
    /// The task and the thing that went wrong, so the user can judge it.
    #[serde(default)]
    pub because: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GotchaDraft {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub text: String,
    /// `verdict t2`, `conflict t3`, `escalation t1`.
    #[serde(default)]
    pub source: String,
}

/// How many gotchas one run may write.
///
/// A cap rather than a filter: a run that produces thirty gotchas is describing
/// the repository, not recording what it learned, and the next run's brief would
/// be mostly memory.
pub const MAX_GOTCHAS: usize = 8;

/// How many skills one run may propose.
///
/// Three, and lower than the gotcha cap on purpose. A gotcha is a sentence; a
/// skill is a document the next run will follow. A run that proposes eight is
/// writing a manual, and a manual nobody reviewed is worse than no manual.
pub const MAX_SKILL_PROPOSALS: usize = 3;

/// A skill proposal that was not kept, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedSkill {
    pub name: String,
    pub reason: String,
}

/// Turn the draft's skill proposals into files in the run's own directory.
///
/// Written to `dir`, never to the user's skill store. A run that could write
/// there could change how every future run behaves, and that is not a side effect
/// to take on trust from a model's summary of its own work. The file is the
/// proposal; accepting it is a separate, human action.
///
/// A proposal is dropped when its name could not be a directory, when it has no
/// body, or when it has no `because` — a skill with no stated reason is a skill
/// nobody can judge, and the panel would be asking the user to accept a document
/// on the strength of its title.
pub fn accept_skills(draft: Vec<SkillDraft>, dir: &std::path::Path) -> (Vec<super::skills::Proposal>, Vec<DroppedSkill>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();

    for entry in draft {
        if kept.len() >= MAX_SKILL_PROPOSALS {
            dropped.push(DroppedSkill {
                name: entry.name,
                reason: format!("over the cap of {MAX_SKILL_PROPOSALS}"),
            });
            continue;
        }
        let mut proposal = super::skills::Proposal {
            name: entry.name.trim().to_lowercase(),
            description: entry.description.trim().to_string(),
            body: entry.body.trim().to_string(),
            because: entry.because.trim().to_string(),
            path: String::new(),
        };
        if !proposal.valid_name() {
            dropped.push(DroppedSkill {
                name: entry.name,
                reason: "the name is not lowercase-hyphenated, so it cannot be a skill directory"
                    .into(),
            });
            continue;
        }
        if proposal.body.is_empty() {
            dropped.push(DroppedSkill {
                name: proposal.name,
                reason: "no body — a skill with no method in it is a title".into(),
            });
            continue;
        }
        if proposal.because.is_empty() {
            dropped.push(DroppedSkill {
                name: proposal.name,
                reason: "no reason given, so there is nothing to judge it on".into(),
            });
            continue;
        }
        if proposal.description.is_empty() {
            // The description is what the ranking reads. A skill without one is
            // invisible to every future run's shortlist, which makes it a file
            // nobody will ever load.
            proposal.description = proposal
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim_start_matches('#')
                .trim()
                .to_string();
        }

        let path = dir.join(format!("{}.md", proposal.name));
        if std::fs::create_dir_all(dir).is_err() || std::fs::write(&path, proposal.render()).is_err()
        {
            dropped.push(DroppedSkill {
                name: proposal.name,
                reason: "the proposal could not be written".into(),
            });
            continue;
        }
        proposal.path = path.to_string_lossy().to_string();
        kept.push(proposal);
    }

    (kept, dropped)
}

/// Parse the reflection pass's output.
pub fn parse(body: &str) -> Result<ReflectionDraft, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("the reflection pass wrote nothing".into());
    }
    serde_json::from_str(trimmed).map_err(|error| format!("not a reflection: {error}"))
}

/// A source, split into the kind it names and the task it names.
///
/// Deliberately forgiving about punctuation. The brief asks for `verdict t2`,
/// but the digest a model reads is prose, and a model that has just read
/// `verdict: passed` writes `verdict: t2` or `[verdict t2]`. Rejecting those
/// does not make the citation stricter, it makes the gate fire on formatting:
/// a live run wrote six gotchas, cited every one of them correctly in
/// substance, and lost all six to a colon. The gate exists to reject a gotcha
/// with no evidence, not a gotcha with a stray bracket.
fn parse_source(source: &str) -> Option<(BoardKind, &str)> {
    let cleaned = source.trim().trim_matches(|c: char| {
        matches!(c, '[' | ']' | '(' | ')' | '"' | '\'' | ',' | '.' | ';')
    });
    let mut parts = cleaned.split_whitespace();
    let kind = match parts.next()?.trim_end_matches(':').to_ascii_lowercase().as_str() {
        "verdict" | "verdicts" => BoardKind::Verdict,
        "conflict" | "conflicts" => BoardKind::Conflict,
        "escalation" | "escalations" => BoardKind::Escalation,
        // The digest prints decisions beside verdicts, because a decision is
        // where a run records why it changed course. Rejecting the citation the
        // digest itself offered cost a live run two of its eleven gotchas.
        "decision" | "decisions" => BoardKind::Decision,
        _ => return None,
    };
    let task = parts
        .next()?
        .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | ':' | ',' | '.' | ';'));
    if task.is_empty() {
        return None;
    }
    Some((kind, task))
}

/// Whether the board actually contains the entry a gotcha cites.
fn is_backed(board: &Board, source: &str) -> bool {
    let Some((kind, task)) = parse_source(source) else {
        return false;
    };
    board.entries().iter().any(|entry: &BoardEntry| {
        entry.kind == kind && entry.task.as_deref() == Some(task)
    })
}

/// Why a gotcha did not make it into the record.
///
/// The panel used to say every dropped gotcha was "discarded for citing nothing
/// on the board", which was wrong for the ones the cap turned away and wrong for
/// the ones missing a file. A drop reason that is not the real reason is worse
/// than no reason: it sends the reader to fix the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DropReason {
    /// The cap was already reached when this one was read.
    OverCap,
    /// No file, or no line of text.
    Incomplete,
    /// The cited board entry does not exist.
    Unbacked,
}

impl DropReason {
    pub fn label(self) -> &'static str {
        match self {
            DropReason::OverCap => "over the cap",
            DropReason::Incomplete => "missing a file or a line of text",
            DropReason::Unbacked => "citing nothing on the board",
        }
    }
}

/// A gotcha that was discarded, and why.
///
/// Flattened so the file on disk still reads as a list of gotchas with one extra
/// key, which is what someone opening `gotchas.dropped.json` expects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedGotcha {
    #[serde(flatten)]
    pub gotcha: GotchaDraft,
    pub reason: DropReason,
}

/// Apply the evidence gate and produce the record that gets stored.
///
/// Returns the reflection plus the gotchas that were dropped and why, so the
/// panel can say "two cited nothing on the board, one was over the cap" rather
/// than silently shrinking the list.
pub fn accept(draft: ReflectionDraft, board: &Board) -> (Reflection, Vec<DroppedGotcha>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();

    for gotcha in draft.gotchas {
        if kept.len() >= MAX_GOTCHAS {
            dropped.push(DroppedGotcha {
                gotcha,
                reason: DropReason::OverCap,
            });
            continue;
        }
        let has_text = !gotcha.text.trim().is_empty();
        let has_file = !gotcha.file.trim().is_empty();
        if !has_text || !has_file {
            dropped.push(DroppedGotcha {
                gotcha,
                reason: DropReason::Incomplete,
            });
            continue;
        }
        if !is_backed(board, &gotcha.source) {
            dropped.push(DroppedGotcha {
                gotcha,
                reason: DropReason::Unbacked,
            });
            continue;
        }
        kept.push(Gotcha {
            file: gotcha.file.trim().to_string(),
            text: gotcha.text.trim().to_string(),
            source: gotcha.source.trim().to_string(),
        });
    }

    let summary = if draft.summary.trim().is_empty() {
        format!("{} gotchas recorded.", kept.len())
    } else {
        draft.summary.trim().to_string()
    };

    (
        Reflection {
            at: now(),
            summary,
            gotchas: kept,
            degraded: None,
        },
        dropped,
    )
}

/// A one-line account of why a set of gotchas was discarded.
///
/// Grouped by reason, in the order the reasons are declared, so the note reads
/// "2 citing nothing on the board, 1 over the cap" rather than repeating the same
/// sentence once per gotcha.
pub fn drop_summary(dropped: &[DroppedGotcha]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for reason in [DropReason::Unbacked, DropReason::Incomplete, DropReason::OverCap] {
        let count = dropped.iter().filter(|d| d.reason == reason).count();
        if count > 0 {
            parts.push(format!("{count} {}", reason.label()));
        }
    }
    parts.join(", ")
}

/// A reflection that could not read the sessions, so it read the board only.
///
/// Named rather than silent: a run whose reflection was degraded must say so, or
/// the next run trusts a memory that was built from less than it appears to be.
pub fn degraded(reason: impl Into<String>, board: &Board) -> Reflection {
    let verdicts = board
        .entries()
        .iter()
        .filter(|e| e.kind == BoardKind::Verdict)
        .count();
    Reflection {
        at: now(),
        summary: format!(
            "Reflection ran on the board only: {verdicts} verdict{} recorded, no gotchas written.",
            if verdicts == 1 { "" } else { "s" }
        ),
        gotchas: Vec::new(),
        degraded: Some(reason.into()),
    }
}

/// The gotchas that apply to a set of files, for a future run's brief.
///
/// Matched on the file path, because that is what a worker can act on. A gotcha
/// about `src/rate.rs` is useful to the worker editing `src/rate.rs` and noise to
/// everyone else.
pub fn relevant<'a>(gotchas: &'a [Gotcha], files: &[String]) -> Vec<&'a Gotcha> {
    gotchas
        .iter()
        .filter(|gotcha| {
            files
                .iter()
                .any(|file| file == &gotcha.file || file.ends_with(&gotcha.file))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with_verdict(task: &str) -> Board {
        let mut board = Board::new();
        board.append(BoardEntry::new("v1", BoardKind::Verdict, "passed").about(task));
        board
    }

    fn draft(gotchas: Vec<GotchaDraft>) -> ReflectionDraft {
        ReflectionDraft {
            summary: "the run went fine".into(),
            gotchas,
            skills: Vec::new(),
        }
    }

    fn gotcha(file: &str, text: &str, source: &str) -> GotchaDraft {
        GotchaDraft {
            file: file.into(),
            text: text.into(),
            source: source.into(),
        }
    }

    #[test]
    fn a_gotcha_backed_by_a_verdict_is_kept() {
        let board = board_with_verdict("t2");
        let (reflection, dropped) = accept(
            draft(vec![gotcha("src/a.rs", "the limiter lives here", "verdict t2")]),
            &board,
        );
        assert_eq!(reflection.gotchas.len(), 1);
        assert!(dropped.is_empty());
        assert_eq!(reflection.gotchas[0].source, "verdict t2");
        assert!(reflection.degraded.is_none());
    }

    #[test]
    fn a_gotcha_with_no_source_is_dropped() {
        let board = board_with_verdict("t2");
        let (reflection, dropped) = accept(
            draft(vec![gotcha("src/a.rs", "something plausible", "")]),
            &board,
        );
        assert!(reflection.gotchas.is_empty());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn a_gotcha_citing_a_verdict_that_does_not_exist_is_dropped() {
        let board = board_with_verdict("t2");
        let (reflection, dropped) = accept(
            draft(vec![gotcha("src/a.rs", "invented", "verdict t9")]),
            &board,
        );
        assert!(reflection.gotchas.is_empty());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn a_gotcha_citing_the_wrong_kind_is_dropped() {
        // The board has a verdict about t2, not a conflict.
        let board = board_with_verdict("t2");
        let (reflection, _) = accept(
            draft(vec![gotcha("src/a.rs", "x", "conflict t2")]),
            &board,
        );
        assert!(reflection.gotchas.is_empty());
    }

    #[test]
    fn a_gotcha_with_no_file_is_dropped() {
        let board = board_with_verdict("t2");
        let (reflection, _) = accept(draft(vec![gotcha("", "x", "verdict t2")]), &board);
        assert!(reflection.gotchas.is_empty());
    }

    #[test]
    fn a_gotcha_with_no_text_is_dropped() {
        let board = board_with_verdict("t2");
        let (reflection, _) = accept(draft(vec![gotcha("src/a.rs", "  ", "verdict t2")]), &board);
        assert!(reflection.gotchas.is_empty());
    }

    #[test]
    fn a_decision_is_a_valid_source() {
        // The digest prints decisions beside verdicts, so a model that cites one
        // is citing what it was shown. Rejecting it made the gate fire on the
        // digest's own vocabulary.
        let mut board = Board::new();
        board.append(BoardEntry::new("lead", BoardKind::Decision, "split the module").about("t2"));
        let (reflection, dropped) = accept(
            draft(vec![gotcha("src/a.rs", "the split is load-bearing", "decision t2")]),
            &board,
        );
        assert_eq!(reflection.gotchas.len(), 1);
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_dropped_gotcha_carries_the_reason_it_was_dropped() {
        let board = board_with_verdict("t2");
        // The cap is checked first, so the two that fail for another reason go
        // in before it fills — which is also the order a model writes them in.
        let mut all = vec![
            gotcha("src/a.rs", "invented", "verdict t9"),
            gotcha("", "no file", "verdict t2"),
        ];
        all.extend(
            (0..(MAX_GOTCHAS + 2))
                .map(|i| gotcha(&format!("src/{i}.rs"), "x", "verdict t2")),
        );
        let (_, dropped) = accept(draft(all), &board);
        assert_eq!(dropped.len(), 4);
        assert_eq!(
            dropped.iter().filter(|d| d.reason == DropReason::OverCap).count(),
            2
        );
        assert_eq!(
            dropped.iter().filter(|d| d.reason == DropReason::Unbacked).count(),
            1
        );
        assert_eq!(
            dropped.iter().filter(|d| d.reason == DropReason::Incomplete).count(),
            1
        );
    }

    #[test]
    fn the_drop_summary_names_each_reason_once() {
        let board = board_with_verdict("t2");
        let mut all = vec![
            gotcha("src/a.rs", "invented", "verdict t9"),
            gotcha("src/b.rs", "  ", "verdict t2"),
        ];
        all.extend(
            (0..(MAX_GOTCHAS + 2))
                .map(|i| gotcha(&format!("src/{i}.rs"), "x", "verdict t2")),
        );
        let (_, dropped) = accept(draft(all), &board);
        let summary = drop_summary(&dropped);
        assert_eq!(
            summary,
            "1 citing nothing on the board, 1 missing a file or a line of text, 2 over the cap"
        );
    }

    #[test]
    fn a_conflict_and_an_escalation_are_valid_sources() {
        let mut board = Board::new();
        board.append(BoardEntry::new("lead", BoardKind::Conflict, "x").about("t3"));
        board.append(BoardEntry::new("w1", BoardKind::Escalation, "y").about("t1"));
        let (reflection, _) = accept(
            draft(vec![
                gotcha("src/a.rs", "a", "conflict t3"),
                gotcha("src/b.rs", "b", "escalation t1"),
            ]),
            &board,
        );
        assert_eq!(reflection.gotchas.len(), 2);
    }

    #[test]
    fn the_gotcha_count_is_capped_and_the_overflow_is_reported() {
        let board = board_with_verdict("t2");
        let many: Vec<GotchaDraft> = (0..(MAX_GOTCHAS + 3))
            .map(|i| gotcha(&format!("src/{i}.rs"), "x", "verdict t2"))
            .collect();
        let (reflection, dropped) = accept(draft(many), &board);
        assert_eq!(reflection.gotchas.len(), MAX_GOTCHAS);
        assert_eq!(dropped.len(), 3);
    }

    #[test]
    fn an_empty_summary_is_replaced_with_a_count() {
        let board = board_with_verdict("t2");
        let mut d = draft(vec![gotcha("src/a.rs", "x", "verdict t2")]);
        d.summary = "  ".into();
        let (reflection, _) = accept(d, &board);
        assert!(reflection.summary.contains("1 gotchas"));
    }

    #[test]
    fn a_degraded_reflection_says_so_and_writes_nothing() {
        let board = board_with_verdict("t2");
        let reflection = degraded("session logging is off", &board);
        assert!(reflection.gotchas.is_empty());
        assert_eq!(reflection.degraded.as_deref(), Some("session logging is off"));
        assert!(reflection.summary.contains("board only"));
        assert!(reflection.summary.contains("1 verdict"));
    }

    #[test]
    fn parsing_rejects_an_empty_body() {
        assert!(parse("   ").is_err());
    }

    #[test]
    fn parsing_rejects_prose() {
        assert!(parse("The run went well and I learned a lot.").is_err());
    }

    #[test]
    fn parsing_accepts_the_documented_shape() {
        let body = r#"{"summary":"ok","gotchas":[{"file":"src/a.rs","text":"t","source":"verdict t1"}]}"#;
        let draft = parse(body).unwrap();
        assert_eq!(draft.gotchas.len(), 1);
        assert_eq!(draft.gotchas[0].source, "verdict t1");
    }

    #[test]
    fn parsing_tolerates_missing_optional_fields() {
        let draft = parse(r#"{"gotchas":[{"file":"a","text":"t","source":"verdict t1"}]}"#).unwrap();
        assert_eq!(draft.summary, "");
        assert_eq!(draft.gotchas.len(), 1);
    }

    #[test]
    fn relevant_gotchas_match_on_the_file_path() {
        let gotchas = vec![
            Gotcha {
                file: "src/rate.rs".into(),
                text: "a".into(),
                source: "verdict t1".into(),
            },
            Gotcha {
                file: "src/other.rs".into(),
                text: "b".into(),
                source: "verdict t2".into(),
            },
        ];
        let hit = relevant(&gotchas, &["src/rate.rs".to_string()]);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].text, "a");

        let none = relevant(&gotchas, &["src/third.rs".to_string()]);
        assert!(none.is_empty());
    }

    #[test]
    fn a_source_parses_only_for_the_kinds_the_digest_prints() {
        assert!(parse_source("verdict t1").is_some());
        assert!(parse_source("conflict t1").is_some());
        assert!(parse_source("escalation t1").is_some());
        assert!(parse_source("decision t1").is_some());
        // A note is not evidence: the digest does not print one, so a gotcha
        // citing one is citing something it was never shown.
        assert!(parse_source("note t1").is_none());
        assert!(parse_source("verdict").is_none());
        assert!(parse_source("").is_none());
    }

    /// The punctuation a model actually writes.
    ///
    /// A live run lost six gotchas to a colon: the digest it read said
    /// `verdict: passed`, so it cited `verdict: t1`. The gate is meant to
    /// reject a gotcha with no evidence, not one with a stray bracket.
    #[test]
    fn a_source_tolerates_the_punctuation_a_model_copies() {
        for source in [
            "verdict: t1",
            "[verdict t1]",
            "verdict t1.",
            "Verdict t1",
            "verdict  t1",
            "verdict t1,",
            "(verdict t1)",
            "\"verdict t1\"",
        ] {
            let parsed = parse_source(source);
            assert!(parsed.is_some(), "{source} should parse");
            let (kind, task) = parsed.unwrap();
            assert_eq!(kind, BoardKind::Verdict, "{source}");
            assert_eq!(task, "t1", "{source}");
        }
    }

    /// Forgiving about punctuation is not forgiving about evidence.
    #[test]
    fn a_tolerated_source_still_has_to_name_a_real_entry() {
        let board = board_with_verdict("t1");
        let kept = accept(
            draft(vec![
                gotcha("src/a.rs", "the limiter owns the rate", "verdict: t1"),
                gotcha("src/b.rs", "the limiter owns the rate", "verdict: t9"),
            ]),
            &board,
        );
        assert_eq!(kept.0.gotchas.len(), 1);
        assert_eq!(kept.1.len(), 1);
    }

    /* -------------------------------------------------------------- *
     * Skill proposals
     * -------------------------------------------------------------- */

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vd-propose-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn proposal(name: &str, body: &str, because: &str) -> SkillDraft {
        SkillDraft {
            name: name.into(),
            description: "a method".into(),
            body: body.into(),
            because: because.into(),
        }
    }

    #[test]
    fn a_proposal_is_written_into_the_runs_own_directory() {
        let dir = scratch("written");
        let (kept, dropped) = accept_skills(
            vec![proposal("scene-graph", "Build the graph first.", "t2 invented it")],
            &dir,
        );
        assert!(dropped.is_empty());
        assert_eq!(kept.len(), 1);
        let path = std::path::PathBuf::from(&kept[0].path);
        assert!(path.is_file());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("---\nname: scene-graph\n"));
        assert!(body.contains("Build the graph first."));
    }

    #[test]
    fn a_proposal_with_no_reason_is_dropped() {
        let dir = scratch("noreason");
        let (kept, dropped) = accept_skills(vec![proposal("x-skill", "body", "")], &dir);
        assert!(kept.is_empty());
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].reason.contains("nothing to judge"));
    }

    #[test]
    fn a_proposal_with_no_body_is_dropped() {
        let dir = scratch("nobody");
        let (kept, dropped) = accept_skills(vec![proposal("x-skill", "   ", "t1")], &dir);
        assert!(kept.is_empty());
        assert!(dropped[0].reason.contains("no body"));
    }

    #[test]
    fn a_name_that_could_escape_the_directory_is_dropped() {
        let dir = scratch("escape");
        let (kept, dropped) = accept_skills(vec![proposal("../evil", "body", "t1")], &dir);
        assert!(kept.is_empty());
        assert!(dropped[0].reason.contains("lowercase-hyphenated"));
    }

    #[test]
    fn the_cap_is_three_and_the_rest_are_reported() {
        let dir = scratch("cap");
        let drafts: Vec<SkillDraft> = (0..5)
            .map(|index| proposal(&format!("skill-{index}"), "body", "t1"))
            .collect();
        let (kept, dropped) = accept_skills(drafts, &dir);
        assert_eq!(kept.len(), MAX_SKILL_PROPOSALS);
        assert_eq!(dropped.len(), 2);
        assert!(dropped[0].reason.contains("over the cap"));
    }

    #[test]
    fn a_missing_description_is_taken_from_the_bodys_first_line() {
        // The description is what the ranking reads. A proposal without one is
        // invisible to every future run's shortlist.
        let dir = scratch("nodesc");
        let mut draft = proposal("scene-graph", "# Build the graph first\n\nThen render.", "t2");
        draft.description = String::new();
        let (kept, _) = accept_skills(vec![draft], &dir);
        assert_eq!(kept[0].description, "Build the graph first");
    }

    #[test]
    fn a_draft_with_no_skills_key_parses_as_no_proposals() {
        // Every reflection written before this field existed.
        let draft = parse(r#"{"summary":"fine","gotchas":[]}"#).unwrap();
        assert!(draft.skills.is_empty());
    }
}
