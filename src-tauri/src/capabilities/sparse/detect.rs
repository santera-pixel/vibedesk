//! What kind of object is this, and is it big enough to be worth negotiating?
//!
//! Ported from `sparseread/core/detector.py`. The thresholds are the upstream
//! ones because they were tuned against real episodes: 4 KiB for a document,
//! 1 KiB for a structured file (a 2 KiB CSV already has more rows than anyone
//! reads), and 3 files or 4 KiB for a directory.

use std::path::{Path, PathBuf};

/// Extensions the upstream reader can actually produce evidence from.
const STRUCTURED_EXTS: &[&str] = &[".csv", ".tsv", ".xlsx", ".json", ".yaml", ".yml", ".xml"];
const TEXT_EXTS: &[&str] = &[
    ".pdf", ".txt", ".md", ".markdown", ".rst", ".html", ".htm", ".log", ".toml", ".ini", ".cfg",
    ".conf",
];

/// Source code.
///
/// Supported, because a directory of source files is a legitimate collection
/// and reporting it as an unsupported type is a lie the panel then repeats.
/// Never carded, because editing needs the exact bytes — the gate keeps every
/// one of these on native tools.
pub const CODE_EXTS: &[&str] = &[
    ".py", ".sh", ".bash", ".zsh", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts",
    ".rs", ".go", ".java", ".kt", ".kts", ".scala", ".c", ".h", ".cc", ".cpp", ".cxx", ".hpp",
    ".hh", ".cs", ".rb", ".php", ".swift", ".lua", ".sql", ".css", ".scss", ".sass", ".less",
    ".vue", ".svelte", ".dart", ".ex", ".exs", ".erl", ".hs", ".ml", ".clj", ".zig", ".nim",
    ".jl", ".pl", ".pm", ".gradle", ".cmake", ".mk", ".bat", ".ps1",
];

/// Directories that are never part of a collection the user meant to read.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".nanobot",
    ".openclaw",
    ".opencode",
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".ruff_cache",
    "memory",
    "sessions",
    "bootstrap",
    "skills",
];

/// Agent scaffolding files. Reading them as evidence is always a mistake.
const SKIP_FILES: &[&str] = &[
    "AGENTS.md",
    "BOOTSTRAP.md",
    "HEARTBEAT.md",
    "IDENTITY.md",
    "SOUL.md",
    "TOOLS.md",
    "USER.md",
];

pub const DEFAULT_LARGE_BYTES: u64 = 4096;
pub const DEFAULT_STRUCTURED_LARGE_BYTES: u64 = 1024;
pub const DEFAULT_COLLECTION_FILES: usize = 3;

/// The shape of the object a read was aimed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub path: PathBuf,
    /// `collection`, or the extension-derived kind (`text`, `csv`, `json`, ...).
    pub kind: String,
    pub size_bytes: u64,
    pub structured: bool,
    pub supported: bool,
    pub large: bool,
    /// Only populated for collections: the files that would be read.
    pub members: Vec<PathBuf>,
}

impl FileInfo {
    pub fn is_collection(&self) -> bool {
        self.kind == "collection"
    }

    pub fn extension(&self) -> String {
        self.path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
            .unwrap_or_default()
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}

/// The upstream kind vocabulary, which the gate's type sets are written against.
pub fn file_type(path: &Path) -> String {
    let suffix = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if CODE_EXTS.contains(&suffix.as_str()) {
        return "code".into();
    }
    match suffix.as_str() {
        ".yml" => "yaml".into(),
        ".md" | ".markdown" | ".rst" => "text".into(),
        ".html" | ".htm" => "html".into(),
        ".log" | ".toml" | ".ini" | ".cfg" | ".conf" => "text".into(),
        ".tsv" => "csv".into(),
        "" => "text".into(),
        other => other.trim_start_matches('.').to_string(),
    }
}

fn is_supported(path: &Path) -> bool {
    let suffix = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    STRUCTURED_EXTS.contains(&suffix.as_str())
        || TEXT_EXTS.contains(&suffix.as_str())
        || CODE_EXTS.contains(&suffix.as_str())
        || name.starts_with("readme")
}

fn is_collection_member(path: &Path) -> bool {
    let suffix = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    is_supported(path) || suffix == ".eml" || name.starts_with("readme")
}

/// Inspect a path without reading its contents.
///
/// A directory is inspected as a collection: the members are enumerated once
/// here so the gate and the preview agree on what "the collection" is.
pub fn inspect(path: &Path) -> FileInfo {
    if path.is_dir() {
        let members = collect_members(path);
        let size: u64 = members
            .iter()
            .filter_map(|member| std::fs::metadata(member).ok())
            .map(|meta| meta.len())
            .sum();
        return FileInfo {
            path: path.to_path_buf(),
            kind: "collection".into(),
            size_bytes: size,
            structured: false,
            supported: !members.is_empty(),
            large: members.len() >= DEFAULT_COLLECTION_FILES || size >= DEFAULT_LARGE_BYTES,
            members,
        };
    }

    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let structured = STRUCTURED_EXTS.contains(
        &path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
            .unwrap_or_default()
            .as_str(),
    );
    let threshold = if structured {
        DEFAULT_STRUCTURED_LARGE_BYTES
    } else {
        DEFAULT_LARGE_BYTES
    };
    FileInfo {
        path: path.to_path_buf(),
        kind: file_type(path),
        size_bytes: size,
        structured,
        supported: is_supported(path),
        large: size >= threshold,
        members: Vec::new(),
    }
}

/// Inspect a path whose bytes we already have.
///
/// The proxy sees a `cat` of a file the agent named relatively, and it has no
/// idea what the agent's working directory is — so a `stat` would fail and the
/// gate would wave through exactly the read it exists to intercept. The bytes
/// are right there in the tool result, so the size comes from them.
pub fn inspect_content(path: &Path, size_bytes: u64) -> FileInfo {
    let structured = STRUCTURED_EXTS.contains(
        &path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
            .unwrap_or_default()
            .as_str(),
    );
    let threshold = if structured {
        DEFAULT_STRUCTURED_LARGE_BYTES
    } else {
        DEFAULT_LARGE_BYTES
    };
    FileInfo {
        path: path.to_path_buf(),
        kind: file_type(path),
        size_bytes,
        structured,
        supported: is_supported(path),
        large: size_bytes >= threshold,
        members: Vec::new(),
    }
}

fn collect_members(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, &mut out, 0);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    // A collection is a directory the user pointed at, not a filesystem scan.
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &path, out, depth + 1);
            continue;
        }
        if SKIP_FILES.contains(&name.as_str()) {
            continue;
        }
        if is_collection_member(&path) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_markdown_file_is_a_text_document() {
        assert_eq!(file_type(Path::new("notes.md")), "text");
        assert_eq!(file_type(Path::new("notes.markdown")), "text");
        assert_eq!(file_type(Path::new("notes.rst")), "text");
    }

    #[test]
    fn a_yml_file_reports_as_yaml_not_yml() {
        assert_eq!(file_type(Path::new("ci.yml")), "yaml");
        assert_eq!(file_type(Path::new("ci.yaml")), "yaml");
    }

    #[test]
    fn a_tsv_file_reports_as_csv_because_the_reader_is_the_same() {
        assert_eq!(file_type(Path::new("rows.tsv")), "csv");
    }

    #[test]
    fn an_unknown_extension_reports_itself_without_the_dot() {
        assert_eq!(file_type(Path::new("thing.parquet")), "parquet");
    }

    #[test]
    fn source_code_is_supported_and_typed_as_code() {
        for name in [
            "page.js", "app.tsx", "lib.rs", "main.go", "module.py", "run.sh", "style.css",
        ] {
            let path = Path::new(name);
            assert_eq!(file_type(path), "code", "{name} should be typed as code");
            assert!(is_supported(path), "{name} should be a supported type");
        }
    }

    #[test]
    fn a_directory_of_source_files_is_a_collection() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-detect-code-collection");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("page.js"), "export const a = 1;\n").unwrap();
        std::fs::write(dir.join("app.ts"), "export const b = 2;\n").unwrap();
        std::fs::write(dir.join("node_modules/dep.js"), "vendored").unwrap();

        let info = inspect(&dir);
        assert!(
            info.supported,
            "a source directory is not an unsupported type"
        );
        let names: Vec<String> = info
            .members
            .iter()
            .map(|member| member.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["app.ts", "page.js"]);
    }

    #[test]
    fn a_structured_file_is_large_at_one_kilobyte() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-detect-structured");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rows.csv");
        std::fs::write(&path, "a,b\n".repeat(300)).unwrap();
        let info = inspect(&path);
        assert!(info.structured);
        assert!(info.large, "1.2 KiB of CSV is already more rows than anyone reads");
    }

    #[test]
    fn a_small_text_file_is_not_large() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-detect-small");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, "# hi\n").unwrap();
        let info = inspect(&path);
        assert!(!info.large);
        assert!(info.supported);
    }

    #[test]
    fn a_directory_is_a_collection_of_its_supported_members() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-detect-collection");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("a.md"), "one").unwrap();
        std::fs::write(dir.join("b.csv"), "x,y\n").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "scaffolding").unwrap();
        std::fs::write(dir.join("node_modules/dep.md"), "vendored").unwrap();
        std::fs::write(dir.join("image.png"), "binary").unwrap();

        let info = inspect(&dir);
        assert!(info.is_collection());
        let names: Vec<String> = info
            .members
            .iter()
            .map(|member| member.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.md", "b.csv"]);
    }

    #[test]
    fn a_readme_without_an_extension_is_still_supported() {
        let dir = std::env::temp_dir().join("vibedesk-sparse-detect-readme");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("README");
        std::fs::write(&path, "hello").unwrap();
        assert!(inspect(&path).supported);
    }
}
