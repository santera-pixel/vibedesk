//! Sparse reading at the `fs/read_text_file` boundary.
//!
//! VibeDesk answers `fs/read_text_file` itself, so this is the one place where
//! the app can decide what a file read costs the agent's context. The pipeline
//! is upstream SparseReading's, in upstream's order:
//!
//! ```text
//! inspect → benefit gate → denoise → card → render
//! ```
//!
//! The gate is the important part. It says *no* far more often than yes: code,
//! config, small files, single-table computation and unsupported shapes all
//! pass through untouched. Only long documents and multi-file evidence bundles
//! are intercepted.
//!
//! ## The loop this closes
//!
//! Upstream's `sro_read` is an MCP tool, so its agent can ask for a targeted
//! read. ACP gives the client no way to add tools, so VibeDesk cannot offer
//! `sro_read`. What it can do is use the paging parameters the read tool
//! already has: every card ends with concrete `line`/`limit` calls, and any
//! request that already carries `line` or `limit` is passed through untouched.
//! The agent therefore always has an exact path to the bytes, and the second
//! read of a file is never carded again.

pub mod denoise;
pub mod detect;
pub mod gate;
pub mod preview;
pub mod units;

use std::path::Path;

pub use gate::{Decision, GateContext, Mode};
pub use preview::Card;

/// How the agent's read tool spells "give me a slice".
///
/// The card ends with the exact calls that fetch the bytes, and those calls are
/// only useful if they name a tool the agent actually has. Vibe's read tool is
/// `read_file` with a zero-based `offset`; ACP's is `fs/read_text_file` with a
/// one-based `line`. A card that names the wrong one is a dead end dressed up as
/// a next step, which is the failure this repository is named after.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTool {
    pub name: String,
    /// The parameter that selects the first line.
    pub offset: String,
    /// The parameter that selects how many lines.
    pub limit: String,
    /// What the offset counts from: 0 for `read_file`, 1 for `fs/read_text_file`.
    pub offset_base: u32,
}

impl ReadTool {
    /// The ACP file-read method, which is what the app answers itself.
    pub fn acp() -> Self {
        Self {
            name: "fs/read_text_file".to_string(),
            offset: "line".to_string(),
            limit: "limit".to_string(),
            offset_base: 1,
        }
    }

    /// Vibe's own read tool, which is what the agent actually calls.
    pub fn agent(name: &str) -> Self {
        Self {
            name: name.to_string(),
            offset: "offset".to_string(),
            limit: "limit".to_string(),
            offset_base: 0,
        }
    }

    /// The offset value that starts at `line`, in this tool's numbering.
    ///
    /// `line` is one-based, as a human counts lines. A one-based tool wants
    /// `line` back; a zero-based tool wants `line - 1`.
    fn offset_for(&self, line: usize) -> usize {
        (line.saturating_sub(1) as u32 + self.offset_base) as usize
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::acp()
    }
}

/// What the boundary should hand back, and what it cost.
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    /// The text to return to the agent.
    pub content: String,
    pub decision: Decision,
    /// True when the raw file body was withheld.
    pub intercepted: bool,
    pub raw_chars: usize,
    pub out_chars: usize,
    pub raw_bytes: u64,
    pub out_bytes: u64,
    pub denoise_rules: Vec<&'static str>,
    /// Present whenever a card was built, so the panel can show it.
    pub card: Option<Card>,
}

impl ReadOutcome {
    pub fn saved_chars(&self) -> usize {
        self.raw_chars.saturating_sub(self.out_chars)
    }
}

/// Decide what a read of `path` should return.
///
/// `paging` is true when the request already carried `line` or `limit`. That is
/// the agent asking for exact bytes, which is the whole point of the card, so
/// it is never second-guessed.
pub fn apply(path: &Path, content: String, paging: bool) -> ReadOutcome {
    apply_with(path, content, paging, &ReadTool::default())
}

/// The same decision, with the card naming the tool the caller's agent has.
pub fn apply_with(
    path: &Path,
    content: String,
    paging: bool,
    tool: &ReadTool,
) -> ReadOutcome {
    let info = detect::inspect(path);
    decide(info, content, paging, tool)
}

/// The same decision for bytes that did not come from a `stat`.
///
/// A `cat` through the agent's shell names a path the proxy cannot resolve — it
/// does not know the agent's working directory — but it does have the bytes. The
/// size comes from them, so the gate still sees a long document as long.
pub fn apply_content(
    path: &Path,
    content: String,
    paging: bool,
    tool: &ReadTool,
) -> ReadOutcome {
    let info = detect::inspect_content(path, content.len() as u64);
    decide(info, content, paging, tool)
}

fn decide(info: detect::FileInfo, content: String, paging: bool, tool: &ReadTool) -> ReadOutcome {
    let decision = gate::decide(&info, &GateContext::default());
    let raw_chars = content.chars().count();
    let raw_bytes = content.len() as u64;

    let pass_through = |content: String| ReadOutcome {
        out_chars: content.chars().count(),
        out_bytes: content.len() as u64,
        content,
        decision: decision.clone(),
        intercepted: false,
        raw_chars,
        raw_bytes,
        denoise_rules: Vec::new(),
        card: None,
    };

    if paging || decision.mode == Mode::Native {
        return pass_through(content);
    }

    // Denoise before previewing so the card describes the text the agent would
    // actually get, not the carrier noise around it.
    let denoised = denoise::denoise(&content, &info.kind);
    let card = preview::build(&info, &decision, &denoised.text, tool);
    let rendered = card.render();

    let content = match decision.mode {
        Mode::Force => rendered,
        // Advisory means the gate declined to make the call. Show the shape,
        // then the content: nothing is withheld, and the agent still gets the
        // map before the territory.
        Mode::Advisory => format!("{rendered}\n---\n{content}"),
        Mode::Native => unreachable!("native was handled above"),
    };

    ReadOutcome {
        out_chars: content.chars().count(),
        out_bytes: content.len() as u64,
        content,
        decision,
        intercepted: true,
        raw_chars,
        raw_bytes,
        denoise_rules: denoised.rules,
        card: Some(card),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("vibedesk-sparse-apply");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_small_file_is_returned_byte_for_byte() {
        let path = write("small.md", "# hi\n\nbody\n");
        let outcome = apply(&path, "# hi\n\nbody\n".into(), false);
        assert_eq!(outcome.content, "# hi\n\nbody\n");
        assert!(!outcome.intercepted);
        assert_eq!(outcome.decision.mode, Mode::Native);
    }

    #[test]
    fn a_long_document_is_replaced_by_a_card() {
        let body = format!("# Spec\n\n{}", "paragraph text here\n\n".repeat(900));
        let path = write("long.md", &body);
        let outcome = apply(&path, body.clone(), false);
        assert!(outcome.intercepted);
        assert_eq!(outcome.decision.mode, Mode::Force);
        assert!(outcome.content.starts_with("[sparse-read]"));
        assert!(outcome.content.contains("fs/read_text_file"));
        assert!(outcome.out_chars < outcome.raw_chars);
        assert!(outcome.saved_chars() > 0);
    }

    #[test]
    fn the_card_names_the_tool_the_agent_actually_has() {
        // The card's whole job is to end with calls the agent can make. Vibe's
        // read tool is `read_file` with a zero-based `offset`; a card that says
        // `fs/read_text_file { line: 1 }` is a dead end dressed as a next step.
        let body = format!("# Spec\n\n{}", "paragraph text here\n\n".repeat(900));
        let path = write("agent-tool.md", &body);
        let tool = ReadTool::agent("read_file");
        let outcome = apply_with(&path, body.clone(), false, &tool);
        assert!(outcome.intercepted);
        assert!(
            outcome.content.contains("read_file { path, offset: 0,"),
            "the first page must start at offset 0, got:\n{}",
            outcome.content
        );
        assert!(!outcome.content.contains("fs/read_text_file"));
        assert!(!outcome.content.contains("line:"));
    }

    #[test]
    fn a_paging_request_is_never_carded() {
        let body = format!("# Spec\n\n{}", "paragraph text here\n\n".repeat(900));
        let path = write("paged.md", &body);
        let outcome = apply(&path, body.clone(), true);
        assert!(!outcome.intercepted);
        assert_eq!(outcome.content, body);
    }

    #[test]
    fn a_large_python_file_is_returned_untouched() {
        let body = "x = 1\n".repeat(20_000);
        let path = write("module.py", &body);
        let outcome = apply(&path, body.clone(), false);
        assert!(!outcome.intercepted);
        assert_eq!(outcome.content, body);
        assert_eq!(outcome.decision.code, "native_code_or_config");
    }

    #[test]
    fn an_advisory_read_keeps_the_content_and_prepends_the_card() {
        let mut body = String::from("id,name\n");
        for index in 0..4000 {
            body.push_str(&format!("{index},item{index}\n"));
        }
        let path = write("rows.csv", &body);
        let outcome = apply(&path, body.clone(), false);
        assert!(outcome.intercepted);
        assert_eq!(outcome.decision.mode, Mode::Advisory);
        assert!(outcome.content.starts_with("[sparse-read]"));
        assert!(outcome.content.ends_with(&body));
    }

    #[test]
    fn the_card_describes_the_denoised_text_not_the_raw_bytes() {
        let mut body = String::new();
        for second in 0..400 {
            body.push_str(&format!("2024-01-01T00:00:{:02}Z heartbeat ok\n", second % 60));
        }
        let path = write("app.log", &body);
        let outcome = apply(&path, body.clone(), false);
        assert!(outcome.denoise_rules.contains(&"volatile_prefix"));
        let card = outcome.card.expect("a card was built");
        assert!(card.render().contains("repeated line with volatile prefix"));
    }

    #[test]
    fn a_missing_file_is_treated_as_an_unsupported_shape_not_an_error() {
        let path = PathBuf::from("/nonexistent/vibedesk/nothing.md");
        let outcome = apply(&path, "content".into(), false);
        assert!(!outcome.intercepted);
        assert_eq!(outcome.content, "content");
    }
}
