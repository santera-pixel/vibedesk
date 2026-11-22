//! The benefit gate: does sparse reading actually pay for this object?
//!
//! Ported from `sparseread/core/benefit_gate.py`. This is the part of
//! SparseReading worth keeping above all else — it is the accumulated answer to
//! "when is a preview cheaper than the file", and it is mostly *no*. Code and
//! config stay native. Small objects stay native. Exact computation over one
//! table stays native. Only long documents and multi-file evidence bundles get
//! intercepted.
//!
//! The three modes map onto three behaviours at the boundary:
//!
//! | mode | action | what the agent receives |
//! | --- | --- | --- |
//! | `force_sro` | intercept | the card and preview, with paging instructions |
//! | `advisory` | nudge | the card and preview, then the content |
//! | `native` | pass | the content, unchanged |

use super::detect::FileInfo;

/// What the gate decided to do with the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Sparse reading has a proven win here; withhold the dump.
    Force,
    /// Sparse reading might help; show the shape, then the content.
    Advisory,
    /// Native tools are cheaper; do not interfere.
    Native,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Force => "force_sro",
            Mode::Advisory => "advisory",
            Mode::Native => "native",
        }
    }

    /// The upstream action vocabulary, which is what the panel displays.
    pub fn action(self) -> &'static str {
        match self {
            Mode::Force => "intercept",
            Mode::Advisory => "nudge",
            Mode::Native => "pass",
        }
    }
}

/// The semantic hint a host model can supply at an episode boundary.
///
/// ACP gives the client no way to attach one to `fs/read_text_file`, so in
/// VibeDesk this is always [`GateContext::default`] — the gate then decides on
/// file shape alone, which is the conservative branch of every rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateContext {
    pub goal: Goal,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Goal {
    SelectiveRead,
    CrossFileEvidence,
    FullFidelity,
    StructuredCompute,
    EditOrExecute,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Coverage {
    Selective,
    Exhaustive,
    #[default]
    Unknown,
}

/// The gate's verdict, carrying the reason so the panel can explain itself.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub mode: Mode,
    pub reason: &'static str,
    pub confidence: f32,
    pub recommended_mode: &'static str,
    pub code: &'static str,
    pub preview_recommended: bool,
    pub scope_kind: &'static str,
}

impl Decision {
    pub fn action(&self) -> &'static str {
        self.mode.action()
    }
}

const TEXT_TYPES: &[&str] = &["pdf", "text", "txt", "md", "markdown", "rst", "html", "htm"];
const TEXT_KINDS: &[&str] = &["text", "txt", "md", "markdown", "rst", "html", "htm", "eml"];
const STRUCTURED_KINDS: &[&str] = &["csv", "tsv", "xls", "xlsx", "json", "yaml", "xml"];
const TABULAR_KINDS: &[&str] = &["csv", "tsv", "xls", "xlsx"];
const CODE_KINDS: &[&str] = &["code"];

/// Configuration files. Reading these sparsely is a net loss for the same
/// reason as source code: the agent needs the exact bytes to edit them.
const NATIVE_CONFIG_EXTS: &[&str] = &[".toml", ".ini", ".cfg", ".conf"];

/// Whether this extension is source code or configuration, which the gate
/// always leaves on native tools.
fn is_native_code_or_config(extension: &str) -> bool {
    super::detect::CODE_EXTS.contains(&extension) || NATIVE_CONFIG_EXTS.contains(&extension)
}

/// These floors sit below the positive upstream benchmark cases (75K PDF, 100K
/// text, 17K audit bundle) while avoiding protocol tax on tiny inputs.
const DOCUMENT_FORCE_BYTES: u64 = 4096;
const COLLECTION_FORCE_BYTES: u64 = 8192;
const TEXT_COLLECTION_FORCE_BYTES: u64 = 24_576;
const COLLECTION_FORCE_FILES: usize = 8;

fn decision(
    mode: Mode,
    reason: &'static str,
    confidence: f32,
    recommended_mode: &'static str,
    code: &'static str,
    preview_recommended: bool,
    scope_kind: &'static str,
) -> Decision {
    Decision {
        mode,
        reason,
        confidence,
        recommended_mode,
        code,
        preview_recommended,
        scope_kind,
    }
}

fn scope_kind(info: &FileInfo) -> &'static str {
    if info.is_collection() {
        return "collection";
    }
    if info.structured {
        return "structured_file";
    }
    if TEXT_TYPES.contains(&info.kind.as_str()) {
        return "single_document";
    }
    "single_file"
}

/// Decide whether to intercept a read of `info`.
pub fn decide(info: &FileInfo, ctx: &GateContext) -> Decision {
    if !info.supported {
        return decision(
            Mode::Native,
            "unsupported type; use native tools",
            1.0,
            "native_read",
            "unsupported",
            false,
            "unsupported",
        );
    }
    if info.is_collection() {
        return decide_collection(info, ctx);
    }
    if !info.large {
        return decision(
            Mode::Native,
            "small object; native path is cheaper than SparseRead negotiation",
            0.95,
            "native_read",
            "below_benefit_floor",
            false,
            scope_kind(info),
        );
    }
    if is_native_code_or_config(&info.extension()) {
        return decision(
            Mode::Native,
            "code or configuration work stays on native tools",
            0.95,
            "native_read",
            "native_code_or_config",
            false,
            "code_or_config",
        );
    }
    if matches!(ctx.goal, Goal::FullFidelity | Goal::EditOrExecute) {
        return decision(
            Mode::Native,
            "the requested operation needs complete source coverage or native execution",
            0.95,
            "native_read",
            "native_goal",
            false,
            scope_kind(info),
        );
    }
    if TEXT_TYPES.contains(&info.kind.as_str()) {
        if info.size_bytes < DOCUMENT_FORCE_BYTES {
            return decision(
                Mode::Advisory,
                "document is near the sparse-reading benefit boundary",
                0.65,
                "sro_optional",
                "document_boundary",
                true,
                "single_document",
            );
        }
        let selective = ctx.goal == Goal::SelectiveRead;
        return decision(
            Mode::Force,
            "long document has a proven sparse-reading path; use collect+slots for multi-fact goals",
            if selective { 0.9 } else { 0.82 },
            "collect_if_multi_fact_else_scout",
            if selective {
                "long_document_selective"
            } else {
                "long_document"
            },
            true,
            "single_document",
        );
    }
    if info.structured {
        if ctx.goal == Goal::StructuredCompute {
            return decision(
                Mode::Native,
                "single-table exact computation is cheaper on native tools",
                0.92,
                "native_read",
                "native_single_table_compute",
                false,
                "structured_file",
            );
        }
        return decision(
            Mode::Advisory,
            "large structured data benefits from SparseRead only for targeted evidence",
            0.7,
            "sro_optional",
            "structured_targeted_only",
            true,
            "structured_file",
        );
    }
    decision(
        Mode::Native,
        "no validated sparse-reading benefit for this resource shape",
        0.8,
        "native_read",
        "no_validated_benefit",
        false,
        scope_kind(info),
    )
}

fn decide_collection(info: &FileInfo, ctx: &GateContext) -> Decision {
    let contains_long_pdf = info
        .members
        .iter()
        .any(|member| {
            member
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase() == "pdf")
                .unwrap_or(false)
                && std::fs::metadata(member)
                    .map(|meta| meta.len() >= DOCUMENT_FORCE_BYTES)
                    .unwrap_or(false)
        });

    if info.members.is_empty() {
        if contains_long_pdf {
            if matches!(
                ctx.goal,
                Goal::FullFidelity | Goal::EditOrExecute | Goal::StructuredCompute
            ) {
                return decision(
                    Mode::Native,
                    "the requested operation needs complete source coverage or native execution",
                    0.95,
                    "native_read",
                    "native_goal",
                    false,
                    "collection",
                );
            }
            return decision(
                Mode::Force,
                "collection contains a long document with a proven sparse-reading path",
                0.88,
                "collect_if_multi_fact_else_scout",
                "collection_long_document",
                true,
                "collection",
            );
        }
        return decision(
            Mode::Native,
            "empty or unsupported collection; use native tools",
            1.0,
            "native_read",
            "empty_collection",
            false,
            "collection",
        );
    }

    let kinds: Vec<String> = info
        .members
        .iter()
        .map(|member| super::detect::file_type(member))
        .collect();
    let count = kinds.len();
    let total_size = info.size_bytes;
    let text_count = kinds.iter().filter(|k| TEXT_KINDS.contains(&k.as_str())).count();
    let structured_count = kinds
        .iter()
        .filter(|k| STRUCTURED_KINDS.contains(&k.as_str()))
        .count();
    let tabular_count = kinds
        .iter()
        .filter(|k| TABULAR_KINDS.contains(&k.as_str()))
        .count();
    let code_count = kinds.iter().filter(|k| CODE_KINDS.contains(&k.as_str())).count();
    let log_count = kinds.iter().filter(|k| k.as_str() == "log").count();

    // A collection of source files is an editing target, not an evidence
    // bundle. The single-file rule keeps code native; a directory of it is the
    // same rule at a larger scale.
    if code_count == count {
        return decision(
            Mode::Native,
            "a collection of source files is an editing target; native tools keep the exact bytes",
            0.95,
            "native_read",
            "native_code_collection",
            false,
            "collection",
        );
    }

    let structured_analysis = count >= 5
        && structured_count >= 3
        && total_size >= 20_000
        && structured_count + text_count + code_count >= 4;
    let edit_structured_prelude =
        structured_analysis && tabular_count >= 3 && structured_count * 2 > count && log_count == 0;

    if ctx.goal == Goal::FullFidelity {
        return decision(
            Mode::Native,
            "the collection goal requires complete source coverage",
            0.95,
            "native_read",
            "native_full_fidelity",
            false,
            "collection",
        );
    }

    if matches!(ctx.goal, Goal::StructuredCompute | Goal::EditOrExecute) {
        if structured_analysis && (ctx.goal == Goal::StructuredCompute || edit_structured_prelude) {
            return decision(
                Mode::Force,
                "multi-file structured inputs benefit from one bounded schema/table plan before native compute or editing",
                0.88,
                "collect",
                "structured_analysis_plan",
                true,
                "collection",
            );
        }
        if ctx.goal == Goal::EditOrExecute {
            return decision(
                Mode::Native,
                "small or mixed editing/execution work is cheaper on native tools",
                0.95,
                "native_read",
                "native_edit_or_execute",
                false,
                "collection",
            );
        }
        return decision(
            Mode::Native,
            "small or single-source exact computation is cheaper on native tools",
            0.9,
            "native_read",
            "native_small_structured_compute",
            false,
            "collection",
        );
    }

    if contains_long_pdf {
        return decision(
            Mode::Force,
            "collection contains a long document with a proven sparse-reading path",
            0.88,
            "collect_if_multi_fact_else_scout",
            "collection_long_document",
            true,
            "collection",
        );
    }

    if ctx.goal == Goal::CrossFileEvidence {
        let evidence_sources = text_count + structured_count + code_count + log_count;
        let structured_only = structured_count == count;
        let broad_exhaustive =
            ctx.coverage == Coverage::Exhaustive && (count >= 10 || total_size >= 40_000);
        if broad_exhaustive && !structured_only {
            return decision(
                Mode::Advisory,
                "broad exhaustive evidence work has token upside but uncertain latency benefit",
                0.7,
                "sro_optional",
                "broad_evidence_boundary",
                true,
                "collection",
            );
        }
        if count >= 3
            && total_size >= COLLECTION_FORCE_BYTES
            && !structured_only
            && evidence_sources >= 3
        {
            return decision(
                Mode::Force,
                "cross-file evidence goal matches a validated collection-reading path",
                0.9,
                "collect",
                "multi_file_evidence",
                true,
                "collection",
            );
        }
        return decision(
            Mode::Advisory,
            "cross-file goal is plausible but the collection is below the force boundary",
            0.68,
            "sro_optional",
            "multi_file_evidence_boundary",
            true,
            "collection",
        );
    }

    let text_heavy = text_count >= std::cmp::max(3, (count + 1) / 2);
    let broad_text_collection =
        text_heavy && (total_size >= TEXT_COLLECTION_FORCE_BYTES || count >= COLLECTION_FORCE_FILES);
    if broad_text_collection {
        return decision(
            Mode::Advisory,
            "large text collection can benefit from source selection once the episode goal is known",
            0.72,
            "sro_optional",
            "large_text_collection_boundary",
            true,
            "collection",
        );
    }

    // Mixed collections are where task intent matters most. Keep preview
    // available, but do not infer audit or computation from filenames.
    let distinct: std::collections::BTreeSet<&str> = kinds.iter().map(String::as_str).collect();
    let mixed = distinct.len() >= 2 || (code_count > 0 && structured_count > 0);
    if mixed || total_size >= COLLECTION_FORCE_BYTES || count >= 3 {
        return decision(
            Mode::Advisory,
            "collection benefit depends on whether the goal is cross-file evidence or native computation",
            0.62,
            "sro_optional",
            "collection_goal_required",
            true,
            "collection",
        );
    }
    decision(
        Mode::Native,
        "small collection; native tools are cheaper",
        0.9,
        "native_read",
        "small_collection",
        false,
        "collection",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn info(kind: &str, size: u64, structured: bool) -> FileInfo {
        FileInfo {
            path: PathBuf::from(format!("object.{kind}")),
            kind: kind.into(),
            size_bytes: size,
            structured,
            supported: true,
            large: size >= if structured { 1024 } else { 4096 },
            members: Vec::new(),
        }
    }

    fn ctx() -> GateContext {
        GateContext::default()
    }

    #[test]
    fn an_unsupported_type_is_passed_through_with_full_confidence() {
        let mut file = info("png", 900_000, false);
        file.supported = false;
        let verdict = decide(&file, &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "unsupported");
        assert_eq!(verdict.confidence, 1.0);
    }

    #[test]
    fn a_small_object_is_left_alone() {
        let verdict = decide(&info("text", 900, false), &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "below_benefit_floor");
    }

    #[test]
    fn a_large_python_file_stays_native_because_editing_needs_exact_bytes() {
        let mut file = info("text", 90_000, false);
        file.path = PathBuf::from("module.py");
        let verdict = decide(&file, &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "native_code_or_config");
    }

    #[test]
    fn a_large_toml_file_stays_native() {
        let mut file = info("text", 90_000, false);
        file.path = PathBuf::from("Cargo.toml");
        assert_eq!(decide(&file, &ctx()).code, "native_code_or_config");
    }

    #[test]
    fn a_large_javascript_file_stays_native_rather_than_reporting_unsupported() {
        let mut file = info("code", 90_000, false);
        file.path = PathBuf::from("page.js");
        let verdict = decide(&file, &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "native_code_or_config");
        assert_eq!(verdict.scope_kind, "code_or_config");
    }

    #[test]
    fn a_collection_of_source_files_is_left_on_native_tools() {
        let mut collection = info("collection", 90_000, false);
        collection.members = vec![
            PathBuf::from("page.js"),
            PathBuf::from("app.ts"),
            PathBuf::from("lib.rs"),
        ];
        let verdict = decide(&collection, &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "native_code_collection");
    }

    #[test]
    fn a_long_markdown_document_is_intercepted() {
        let mut file = info("text", 120_000, false);
        file.path = PathBuf::from("spec.md");
        let verdict = decide(&file, &ctx());
        assert_eq!(verdict.mode, Mode::Force);
        assert_eq!(verdict.code, "long_document");
        assert_eq!(verdict.action(), "intercept");
        assert!(verdict.preview_recommended);
    }

    #[test]
    fn a_document_between_one_and_four_kilobytes_is_only_nudged() {
        let mut file = info("text", 2000, false);
        file.path = PathBuf::from("notes.md");
        file.large = true;
        let verdict = decide(&file, &ctx());
        assert_eq!(verdict.mode, Mode::Advisory);
        assert_eq!(verdict.code, "document_boundary");
        assert_eq!(verdict.action(), "nudge");
    }

    #[test]
    fn a_large_csv_is_only_nudged_because_targeted_evidence_is_the_only_win() {
        let verdict = decide(&info("csv", 400_000, true), &ctx());
        assert_eq!(verdict.mode, Mode::Advisory);
        assert_eq!(verdict.code, "structured_targeted_only");
    }

    #[test]
    fn a_large_csv_under_an_exact_compute_goal_stays_native() {
        let mut context = ctx();
        context.goal = Goal::StructuredCompute;
        let verdict = decide(&info("csv", 400_000, true), &context);
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "native_single_table_compute");
    }

    #[test]
    fn a_full_fidelity_goal_vetoes_interception_even_for_a_long_document() {
        let mut file = info("text", 120_000, false);
        file.path = PathBuf::from("spec.md");
        let mut context = ctx();
        context.goal = Goal::FullFidelity;
        assert_eq!(decide(&file, &context).mode, Mode::Native);
    }

    #[test]
    fn an_unknown_shape_with_no_validated_benefit_stays_native() {
        let verdict = decide(&info("parquet", 900_000, false), &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "no_validated_benefit");
    }

    #[test]
    fn an_empty_collection_is_left_alone() {
        let mut collection = info("collection", 0, false);
        collection.members.clear();
        let verdict = decide(&collection, &ctx());
        assert_eq!(verdict.mode, Mode::Native);
        assert_eq!(verdict.code, "empty_collection");
    }

    #[test]
    fn a_small_mixed_collection_is_only_nudged() {
        let mut collection = info("collection", 3000, false);
        collection.members = vec![PathBuf::from("a.md"), PathBuf::from("b.csv")];
        let verdict = decide(&collection, &ctx());
        assert_eq!(verdict.mode, Mode::Advisory);
        assert_eq!(verdict.code, "collection_goal_required");
    }

    #[test]
    fn a_broad_text_collection_is_nudged_not_intercepted() {
        let mut collection = info("collection", 30_000, false);
        collection.members = (0..9).map(|i| PathBuf::from(format!("doc{i}.md"))).collect();
        let verdict = decide(&collection, &ctx());
        assert_eq!(verdict.mode, Mode::Advisory);
        assert_eq!(verdict.code, "large_text_collection_boundary");
    }

    #[test]
    fn a_cross_file_evidence_goal_intercepts_a_real_bundle() {
        let mut collection = info("collection", 20_000, false);
        collection.members = vec![
            PathBuf::from("a.md"),
            PathBuf::from("b.csv"),
            PathBuf::from("c.json"),
        ];
        let mut context = ctx();
        context.goal = Goal::CrossFileEvidence;
        let verdict = decide(&collection, &context);
        assert_eq!(verdict.mode, Mode::Force);
        assert_eq!(verdict.code, "multi_file_evidence");
    }

    #[test]
    fn a_structured_analysis_goal_intercepts_a_wide_table_bundle() {
        let mut collection = info("collection", 40_000, false);
        collection.members = vec![
            PathBuf::from("a.csv"),
            PathBuf::from("b.csv"),
            PathBuf::from("c.tsv"),
            PathBuf::from("d.json"),
            PathBuf::from("e.md"),
        ];
        let mut context = ctx();
        context.goal = Goal::StructuredCompute;
        let verdict = decide(&collection, &context);
        assert_eq!(verdict.mode, Mode::Force);
        assert_eq!(verdict.code, "structured_analysis_plan");
    }
}
