//! What the code graph actually holds.
//!
//! The panel used to show a binary path and a registration flag, which answers
//! "is it wired up" and not "what is indexed". Those are different questions,
//! and the second one is the one a user asks when they have just watched the
//! agent index a repository.
//!
//! The answers live in the server, so this module asks it: `list_projects` for
//! the repositories it knows, `index_status` for the node and edge counts of
//! one of them, and `query_graph` for the nodes and edges themselves. The first
//! two come back as text tables, so the parsers here are the contract — and they
//! are tested against the exact shapes the server emits.
//!
//! The graph is a different question from the counts. A count says how big the
//! index is; the graph says what is in it, which is what a picture needs. It is
//! also the expensive one — a dozen paged round trips — so it is fetched on
//! demand and cached apart from the counts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::external;
use super::mcp_client::McpClient;
use super::CapabilityId;
use crate::error::{AppError, AppResult};

/// A repository the server has indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// The server's own key for the project, which is what `index_status` wants.
    pub name: String,
    pub root: String,
    pub branch: String,
    /// Filled in by [`state`] when the server answers for this project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
}

/// One project's index, as the server reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub project: String,
    pub nodes: u64,
    pub edges: u64,
    pub status: String,
    pub root: String,
    pub indexed_at: Option<String>,
    /// Files the parser could only partly read.
    pub parse_partial: u64,
    /// Files the parser could not read at all.
    pub parse_unusable: u64,
    pub skipped: u64,
    /// Directories deliberately left out of the graph.
    pub not_indexed_dirs: u64,
    /// Files deliberately left out of the graph.
    pub not_indexed_files: u64,
    /// Whether the server is watching the tree for changes.
    pub watching: bool,
}

/// Everything the code graph panel needs, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeGraphState {
    /// Whether the server could be reached at all.
    pub available: bool,
    /// Why it could not be, when it could not.
    pub reason: Option<String>,
    /// The folder the panel is asking about.
    pub workspace: Option<String>,
    /// Whether that folder is itself one of the indexed projects.
    pub workspace_indexed: bool,
    /// Indexed projects whose root sits inside the workspace.
    ///
    /// A workspace is often a parent folder — `D:\Projects` holding several
    /// repositories — so "is this folder indexed" is the wrong question to ask
    /// on its own. These are the projects the folder actually contains.
    pub contained: Vec<Project>,
    pub projects: Vec<Project>,
    /// The full status of the project this folder resolves to.
    ///
    /// Never an unrelated project: when the folder is neither indexed nor the
    /// parent of exactly one indexed project, this is `None` and the panel says
    /// so rather than showing another repository's numbers.
    pub current: Option<IndexStatus>,
}

impl CodeGraphState {
    pub fn unavailable(reason: String, workspace: Option<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason),
            workspace,
            workspace_indexed: false,
            contained: Vec::new(),
            projects: Vec::new(),
            current: None,
        }
    }
}

/// Parse the `list_projects` table.
///
/// ```text
/// projects: 2  (cols: name root_path branch)
///   D-VibeDesk-VibeDeskProjects-LivingMiniture D:/VibeDesk/VibeDeskProjects/LivingMiniture -
/// total: 2
/// ```
///
/// The root is the middle column and may contain spaces, so the name is the
/// first field and the branch the last, and everything between them is the root.
pub fn parse_projects(text: &str) -> Vec<Project> {
    let mut projects = Vec::new();

    for line in text.lines() {
        if !line.starts_with("  ") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        projects.push(Project {
            name: fields[0].to_string(),
            root: fields[1..fields.len() - 1].join(" "),
            branch: fields[fields.len() - 1].to_string(),
            nodes: None,
            edges: None,
            indexed_at: None,
        });
    }

    projects
}

/// Parse the `index_status` report.
///
/// Top-level keys are scalars; the nested blocks carry the counts that matter
/// (`parse_partial.count`, `not_indexed.files_count`, `watch.watched`).
pub fn parse_status(text: &str) -> Option<IndexStatus> {
    let mut project: Option<String> = None;
    let mut nodes = 0u64;
    let mut edges = 0u64;
    let mut status = String::new();
    let mut root = String::new();
    let mut indexed_at: Option<String> = None;
    let mut parse_partial = 0u64;
    let mut parse_unusable = 0u64;
    let mut skipped = 0u64;
    let mut not_indexed_dirs = 0u64;
    let mut not_indexed_files = 0u64;
    let mut watching = false;

    let mut section = String::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        if !indented {
            if value.is_empty() {
                section = key.to_string();
                continue;
            }
            section.clear();
            match key {
                "project" => project = Some(value.to_string()),
                "nodes" => nodes = value.parse().unwrap_or(0),
                "edges" => edges = value.parse().unwrap_or(0),
                "status" => status = value.to_string(),
                "root_path" => root = value.to_string(),
                "indexed_at" => indexed_at = Some(value.to_string()),
                _ => {}
            }
            continue;
        }

        match (section.as_str(), key) {
            ("parse_partial", "count") => parse_partial = value.parse().unwrap_or(0),
            ("parse_unusable", "count") => parse_unusable = value.parse().unwrap_or(0),
            ("skipped", "count") => skipped = value.parse().unwrap_or(0),
            ("not_indexed", "dirs_count") => not_indexed_dirs = value.parse().unwrap_or(0),
            ("not_indexed", "files_count") => not_indexed_files = value.parse().unwrap_or(0),
            ("watch", "watched") => watching = value == "true",
            _ => {}
        }
    }

    Some(IndexStatus {
        project: project?,
        nodes,
        edges,
        status,
        root,
        indexed_at,
        parse_partial,
        parse_unusable,
        skipped,
        not_indexed_dirs,
        not_indexed_files,
        watching,
    })
}

/// Whether a project root names the same folder as `cwd`.
fn same_root(root: &str, cwd: &Path) -> bool {
    let normalise = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    normalise(root) == normalise(&cwd.display().to_string())
}

/// Whether a project root sits inside `workspace`.
fn is_under(root: &str, workspace: &Path) -> bool {
    let normalise = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let root = normalise(root);
    let workspace = normalise(&workspace.display().to_string());
    root != workspace && root.starts_with(&format!("{workspace}/"))
}

/// How many projects get a status query.
///
/// Each one is a round trip to the server. A user with fifty indexed
/// repositories does not need fifty counts on a panel they opened to check one,
/// and the list still shows every root.
const MAX_STATUS_QUERIES: usize = 8;

/// Ask the server what it holds.
///
/// One process, one call per project, killed on return. A server that is not
/// installed or does not answer is reported as a reason, not an error: the panel
/// says why it is empty instead of showing nothing.
pub fn state(cwd: Option<&Path>) -> CodeGraphState {
    let workspace = cwd.map(|path| path.display().to_string());

    let Some(server) = external::server(CapabilityId::CodeGraph) else {
        return CodeGraphState::unavailable("no code graph server is configured".into(), workspace);
    };
    let Some(binary) = external::find(server.binary) else {
        return CodeGraphState::unavailable(
            format!("{} is not installed", server.binary),
            workspace,
        );
    };

    let args: Vec<String> = server.args.iter().map(|arg| (*arg).to_string()).collect();
    let mut client = match McpClient::start(&binary.display().to_string(), &args, cwd) {
        Ok(client) => client,
        Err(error) => return CodeGraphState::unavailable(error.to_string(), workspace),
    };
    if let Err(error) = client.initialize() {
        return CodeGraphState::unavailable(error.to_string(), workspace);
    }

    let mut projects = client
        .call_tool("list_projects", json!({}))
        .map(|text| parse_projects(&text))
        .unwrap_or_default();

    // One status query per project, capped. The counts belong in the list: a row
    // that names a repository and not its size answers nothing.
    let mut statuses: Vec<Option<IndexStatus>> = Vec::with_capacity(projects.len());
    for project in projects.iter_mut() {
        let status = if statuses.len() < MAX_STATUS_QUERIES {
            client
                .call_tool("index_status", json!({ "project": project.name }))
                .ok()
                .and_then(|text| parse_status(&text))
        } else {
            None
        };
        if let Some(status) = &status {
            project.nodes = Some(status.nodes);
            project.edges = Some(status.edges);
            project.indexed_at = status.indexed_at.clone();
        }
        statuses.push(status);
    }

    let exact = cwd.and_then(|cwd| {
        projects
            .iter()
            .position(|project| same_root(&project.root, cwd))
    });

    let contained_positions: Vec<usize> = match cwd {
        Some(cwd) => projects
            .iter()
            .enumerate()
            .filter(|(_, project)| is_under(&project.root, cwd))
            .map(|(index, _)| index)
            .collect(),
        None => Vec::new(),
    };
    let contained: Vec<Project> = contained_positions
        .iter()
        .filter_map(|index| projects.get(*index).cloned())
        .collect();

    // The folder itself, or the single project inside it. Never an unrelated
    // project: showing another repository's counts under this folder's name is
    // how the panel came to report 128 nodes for a graph of 266.
    let target = exact.or_else(|| {
        if contained_positions.len() == 1 {
            contained_positions.first().copied()
        } else {
            None
        }
    });

    let current = target.and_then(|index| statuses.get(index).cloned().flatten());

    CodeGraphState {
        available: true,
        reason: None,
        workspace,
        workspace_indexed: exact.is_some(),
        contained,
        projects,
        current,
    }
}

/// How long a panel answer is reused.
///
/// The server is a 300 MB binary and a cold start is seconds, so polling it on
/// every render would make the panel the slowest thing in the app. Twenty
/// seconds is short enough that a fresh index shows up while the user is still
/// looking at the panel, and long enough that polling costs nothing.
const CACHE_TTL: Duration = Duration::from_secs(20);

type Cache = Mutex<Option<(Instant, Option<PathBuf>, CodeGraphState)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// [`state`], memoised per workspace.
pub fn cached_state(cwd: Option<&Path>, refresh: bool) -> CodeGraphState {
    let key = cwd.map(Path::to_path_buf);

    if !refresh {
        if let Ok(guard) = cache().lock() {
            if let Some((at, cached_key, value)) = guard.as_ref() {
                if at.elapsed() < CACHE_TTL && cached_key == &key {
                    return value.clone();
                }
            }
        }
    }

    let value = state(cwd);

    if let Ok(mut guard) = cache().lock() {
        *guard = Some((Instant::now(), key, value.clone()));
    }

    value
}

/* ------------------------------------------------------------------ *
 * The graph itself
 * ------------------------------------------------------------------ */

/// One node, in the shape a renderer needs.
///
/// The server carries thirty-odd properties per node; these are the ones a
/// picture can use — what it is called, what kind of thing it is, where it
/// lives, and how connected it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    /// The qualified name, which is also the key edges refer to.
    pub id: String,
    pub name: String,
    /// The first node label: `Function`, `File`, `Module`, and so on.
    pub label: String,
    pub file: String,
    pub line: Option<u32>,
    pub complexity: Option<f64>,
    pub importance: Option<f64>,
    pub entry: bool,
    pub exported: bool,
    pub test: bool,
    pub lines: Option<u32>,
    /// How many edges touch this node, in either direction.
    pub degree: u32,
}

/// One edge, by qualified name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    /// `CALLS`, `DEFINES`, `IMPORTS`, and so on.
    pub kind: String,
}

/// A label or edge type with its exact count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCount {
    pub label: String,
    pub count: u64,
}

/// A whole project's graph, ready to draw.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeGraphGraph {
    pub project: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Every node label the index holds, with its exact count.
    pub labels: Vec<LabelCount>,
    /// Every edge type the index holds, with its exact count.
    pub kinds: Vec<LabelCount>,
    /// The index's own totals, which are exact even when the fetch is not.
    pub total_nodes: u64,
    pub total_edges: u64,
    /// Whether the fetch stopped before it had read the whole graph.
    pub truncated: bool,
    /// What was read, when it was not everything.
    pub truncation: Option<String>,
}

/// One page of a `query_graph` answer.
///
/// The server caps a page by output budget rather than by row count, so a
/// request for four thousand rows can come back with ninety-eight and a cursor.
/// Reading a graph means following that cursor, not raising the limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryPage {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total: u64,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

/// A cell as text.
///
/// The server sends every cell as a string, but a null is a null and a number
/// is a number, and a renderer that has to guess which is which is a renderer
/// that draws `null` as a node name.
fn cell_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse one `query_graph` page.
pub fn parse_query_page(text: &str) -> Option<QueryPage> {
    let value: Value = serde_json::from_str(text).ok()?;
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(cell_text).collect())
        .unwrap_or_default();
    let rows: Vec<Vec<String>> = value
        .get("rows")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(cell_text).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();

    Some(QueryPage {
        total: value
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(rows.len() as u64),
        has_more: value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        next_cursor: value
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(str::to_string),
        columns,
        rows,
    })
}

/// Where a named column sits in a page.
fn column(page: &QueryPage, name: &str) -> Option<usize> {
    page.columns.iter().position(|column| column == name)
}

/// A cell by column, treating an empty cell as absent.
///
/// The server writes an unset property as `""`, and `""` parsed as a number is
/// zero — which would report every node without a complexity as complexity 0.
fn pick(row: &[String], at: Option<usize>) -> Option<String> {
    let value = row.get(at?)?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// The first label of a `labels(n)` cell.
///
/// The server returns the label list as a JSON string inside a JSON string, so
/// it arrives here as `["Function"]` and needs one more parse. A cell that is
/// not a list is taken as the label itself.
pub fn first_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
        if let Some(Value::String(label)) = items.first() {
            return label.clone();
        }
    }
    trimmed
        .trim_matches(|character| character == '[' || character == ']' || character == '"')
        .to_string()
}

/// The nodes in one page.
pub fn nodes_from_page(page: &QueryPage) -> Vec<GraphNode> {
    let Some(id_at) = column(page, "qn") else {
        return Vec::new();
    };
    let name_at = column(page, "name");
    let labels_at = column(page, "labels");
    let file_at = column(page, "file");
    let line_at = column(page, "line");
    let complexity_at = column(page, "complexity");
    let importance_at = column(page, "importance");
    let entry_at = column(page, "entry");
    let exported_at = column(page, "exported");
    let test_at = column(page, "test");
    let lines_at = column(page, "lines");
    let degree_at = column(page, "degree");

    page.rows
        .iter()
        .filter_map(|row| {
            let id = pick(row, Some(id_at))?;
            Some(GraphNode {
                name: pick(row, name_at).unwrap_or_else(|| id.clone()),
                label: pick(row, labels_at)
                    .map(|raw| first_label(&raw))
                    .unwrap_or_else(|| "Node".to_string()),
                file: pick(row, file_at).unwrap_or_default(),
                line: pick(row, line_at).and_then(|value| value.parse().ok()),
                complexity: pick(row, complexity_at).and_then(|value| value.parse().ok()),
                importance: pick(row, importance_at).and_then(|value| value.parse().ok()),
                entry: pick(row, entry_at).as_deref() == Some("true"),
                exported: pick(row, exported_at).as_deref() == Some("true"),
                test: pick(row, test_at).as_deref() == Some("true"),
                lines: pick(row, lines_at).and_then(|value| value.parse().ok()),
                degree: pick(row, degree_at)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                id,
            })
        })
        .collect()
}

/// The edges in one page.
pub fn edges_from_page(page: &QueryPage) -> Vec<GraphEdge> {
    let (Some(source_at), Some(target_at), Some(kind_at)) = (
        column(page, "s"),
        column(page, "t"),
        column(page, "type"),
    ) else {
        return Vec::new();
    };

    page.rows
        .iter()
        .filter_map(|row| {
            Some(GraphEdge {
                source: pick(row, Some(source_at))?,
                target: pick(row, Some(target_at))?,
                kind: pick(row, Some(kind_at))?,
            })
        })
        .collect()
}

/// Drop edges whose endpoints were not read.
///
/// A truncated fetch can return an edge before the node it points at. Keeping
/// it would make the renderer look for a node that is not there, and a graph
/// that draws edges into nothing looks broken rather than partial.
pub fn prune_edges(nodes: &[GraphNode], edges: Vec<GraphEdge>) -> Vec<GraphEdge> {
    let known: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    edges
        .into_iter()
        .filter(|edge| {
            known.contains(edge.source.as_str()) && known.contains(edge.target.as_str())
        })
        .collect()
}

/// The exact label and edge-type counts, from `get_graph_schema`.
///
/// This is the only place the totals are exact: a paged query reports the size
/// of the result set it walked, not the size of the graph.
pub fn parse_schema(text: &str) -> Option<(Vec<LabelCount>, Vec<LabelCount>)> {
    let value: Value = serde_json::from_str(text).ok()?;

    let collect = |key: &str, name: &str| -> Vec<LabelCount> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(LabelCount {
                            label: item.get(name)?.as_str()?.to_string(),
                            count: item.get("count")?.as_u64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    Some((collect("node_labels", "label"), collect("edge_types", "type")))
}

/// How many pages of one query are read before the fetch gives up.
///
/// Each page is a round trip. Thirty is enough for a graph of several thousand
/// nodes and short enough that a pathological index cannot hold the panel.
const MAX_PAGES: usize = 30;

/// The node query.
///
/// Ordered by degree so that a graph too large to read whole still shows the
/// nodes that carry its structure rather than an arbitrary slice of it.
const NODE_QUERY: &str = "MATCH (n) OPTIONAL MATCH (n)-[r]-() \
RETURN n.qualified_name AS qn, labels(n) AS labels, n.name AS name, \
n.file_path AS file, n.start_line AS line, n.complexity AS complexity, \
n.importance AS importance, n.is_entry_point AS entry, \
n.is_exported AS exported, n.is_test AS test, n.lines AS lines, \
count(r) AS degree ORDER BY degree DESC LIMIT 4000";

const EDGE_QUERY: &str = "MATCH (a)-[r]->(b) \
RETURN a.qualified_name AS s, b.qualified_name AS t, type(r) AS type LIMIT 12000";

/// Read every page of one query, following the server's cursor.
///
/// Returns the pages and whether the page ceiling was hit before the cursor ran
/// out.
fn read_pages(
    client: &mut McpClient,
    project: &str,
    query: &str,
) -> AppResult<(Vec<QueryPage>, bool)> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_PAGES {
        let mut arguments = json!({
            "project": project,
            "query": query,
            "format": "json",
            "max_rows": 4000,
        });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }

        let text = client.call_tool("query_graph", arguments)?;
        let Some(page) = parse_query_page(&text) else {
            return Err(AppError::message(
                "the server answered with something that is not a query page",
            ));
        };

        let more = page.has_more;
        cursor = page.next_cursor.clone();
        pages.push(page);

        if !more || cursor.is_none() {
            return Ok((pages, false));
        }
    }

    Ok((pages, true))
}

/// One project's whole graph, for the renderer.
///
/// Three calls to one server process: the schema for exact totals, then the
/// nodes and the edges, each paged to the end. A server that is not installed
/// is an error here rather than an empty graph, because an empty graph and an
/// unreachable server look identical on a canvas and mean opposite things.
pub fn graph(project: &str, cwd: Option<&Path>) -> AppResult<CodeGraphGraph> {
    let server = external::server(CapabilityId::CodeGraph)
        .ok_or_else(|| AppError::message("no code graph server is configured"))?;
    let binary = external::find(server.binary)
        .ok_or_else(|| AppError::message(format!("{} is not installed", server.binary)))?;

    let args: Vec<String> = server.args.iter().map(|arg| (*arg).to_string()).collect();
    let mut client = McpClient::start(&binary.display().to_string(), &args, cwd)?;
    client.initialize()?;

    let (labels, kinds) = client
        .call_tool(
            "get_graph_schema",
            json!({ "project": project, "format": "json" }),
        )
        .ok()
        .and_then(|text| parse_schema(&text))
        .unwrap_or_default();

    let (node_pages, nodes_truncated) = read_pages(&mut client, project, NODE_QUERY)?;
    let (edge_pages, edges_truncated) = read_pages(&mut client, project, EDGE_QUERY)?;

    let nodes: Vec<GraphNode> = node_pages.iter().flat_map(nodes_from_page).collect();
    let edges = prune_edges(
        &nodes,
        edge_pages.iter().flat_map(edges_from_page).collect(),
    );

    let total_nodes = labels.iter().map(|entry| entry.count).sum();
    let total_edges = kinds.iter().map(|entry| entry.count).sum();

    let truncation = (nodes_truncated || edges_truncated).then(|| {
        format!(
            "read {} of {} nodes and {} of {} edges",
            nodes.len(),
            total_nodes,
            edges.len(),
            total_edges
        )
    });

    Ok(CodeGraphGraph {
        project: project.to_string(),
        nodes,
        edges,
        labels,
        kinds,
        total_nodes,
        total_edges,
        truncated: truncation.is_some(),
        truncation,
    })
}

/// How long a graph answer is reused.
///
/// Reading a graph is a dozen round trips to a 300 MB binary, so it is cached
/// far longer than the counts. The panel has a refresh button for the case
/// where the user has just re-indexed.
const GRAPH_TTL: Duration = Duration::from_secs(120);

type GraphCache = Mutex<Option<(Instant, String, CodeGraphGraph)>>;

fn graph_cache() -> &'static GraphCache {
    static CACHE: OnceLock<GraphCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// [`graph`], memoised per project.
pub fn cached_graph(
    project: &str,
    cwd: Option<&Path>,
    refresh: bool,
) -> AppResult<CodeGraphGraph> {
    if !refresh {
        if let Ok(guard) = graph_cache().lock() {
            if let Some((at, cached_project, value)) = guard.as_ref() {
                if at.elapsed() < GRAPH_TTL && cached_project == project {
                    return Ok(value.clone());
                }
            }
        }
    }

    let value = graph(project, cwd)?;

    if let Ok(mut guard) = graph_cache().lock() {
        *guard = Some((Instant::now(), project.to_string(), value.clone()));
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECTS: &str = "projects: 2  (cols: name root_path branch)\n  \
C-Users-Santera-AppData-Local-Temp-vd-game-skate C:/Users/Santera/AppData/Local/Temp/vd-game/skate -\n  \
D-VibeDesk-VibeDeskProjects-LivingMiniture D:/VibeDesk/VibeDeskProjects/LivingMiniture -\n\
total: 2\nreturned: 2\nhas_more: false\n";

    const STATUS: &str = "project: D-VibeDesk-VibeDeskProjects-LivingMiniture\n\
nodes: 266\n\
edges: 733\n\
status: ready\n\
root_path: D:/VibeDesk/VibeDeskProjects/LivingMiniture\n\
indexed_at: 2026-11-18T10:45:55Z\n\
parse_partial:\n  files: 0\n  count: 0\n  truncated: false\n\
parse_unusable:\n  files: 0\n  count: 0\n  truncated: false\n\
skipped:\n  files: 0\n  count: 0\n  truncated: false\n\
not_indexed:\n  dirs: 0\n  dirs_count: 1\n  files: 0\n  files_count: 11\n  truncated: true\n  dirs_omitted: 1\n  files_omitted: 11\n\
cross_repo:\n  status: never_run\n\
doc_links:\n  mentions: 1\n  unresolved:\n    below_bar_tier: 2\n  status: ok\n\
watch:\n  watched: true\n  strategy: pending\n  poll_interval_ms: 5000\n";

    #[test]
    fn the_project_table_parses_into_names_and_roots() {
        let projects = parse_projects(PROJECTS);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[1].name, "D-VibeDesk-VibeDeskProjects-LivingMiniture");
        assert_eq!(projects[1].root, "D:/VibeDesk/VibeDeskProjects/LivingMiniture");
        assert_eq!(projects[1].branch, "-");
    }

    #[test]
    fn a_root_with_spaces_stays_one_column() {
        let text = "projects: 1  (cols: name root_path branch)\n  \
my-project C:/Users/Some One/My Project main\n";
        let projects = parse_projects(text);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].root, "C:/Users/Some One/My Project");
        assert_eq!(projects[0].branch, "main");
    }

    #[test]
    fn the_status_report_parses_counts_and_nested_blocks() {
        let status = parse_status(STATUS).expect("a status");
        assert_eq!(status.nodes, 266);
        assert_eq!(status.edges, 733);
        assert_eq!(status.status, "ready");
        assert_eq!(status.indexed_at.as_deref(), Some("2026-11-18T10:45:55Z"));
        assert_eq!(status.parse_partial, 0);
        assert_eq!(status.parse_unusable, 0);
        assert_eq!(status.not_indexed_dirs, 1);
        assert_eq!(status.not_indexed_files, 11);
        assert!(status.watching);
    }

    #[test]
    fn a_status_without_a_project_is_not_a_status() {
        assert!(parse_status("nodes: 1\nedges: 2\n").is_none());
    }

    #[test]
    fn an_error_payload_is_not_mistaken_for_a_status() {
        let error = r#"{"error":"missing required argument: project"}"#;
        assert!(parse_status(error).is_none());
    }

    #[test]
    fn a_root_matches_whatever_the_separators() {
        assert!(same_root(
            "D:/VibeDesk/VibeDeskProjects/LivingMiniture",
            Path::new(r"D:\VibeDesk\VibeDeskProjects\LivingMiniture")
        ));
        assert!(!same_root(
            "D:/VibeDesk/VibeDeskProjects/LivingMiniture",
            Path::new(r"D:\VibeDesk\VibeDeskProjects\Other")
        ));
    }

    #[test]
    fn an_empty_table_is_an_empty_list_not_a_panic() {
        assert!(parse_projects("projects: 0\n").is_empty());
        assert!(parse_projects("").is_empty());
    }

    #[test]
    fn a_project_inside_the_workspace_is_under_it() {
        let workspace = Path::new(r"D:\VibeDesk\VibeDeskProjects");
        assert!(is_under(
            "D:/VibeDesk/VibeDeskProjects/LivingMiniture",
            workspace
        ));
        assert!(!is_under("D:/VibeDesk/VibeDeskProjects", workspace));
        assert!(!is_under(
            "C:/Users/Santera/AppData/Local/Temp/vd-game/skate",
            workspace
        ));
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_under_it() {
        let workspace = Path::new(r"D:\VibeDesk\VibeDeskProjects");
        assert!(!is_under(
            "D:/VibeDesk/VibeDeskProjects-old/thing",
            workspace
        ));
    }

    /// Diagnostic, not an assertion: what the panel would show on this machine.
    #[test]
    #[ignore]
    fn print_the_code_graph_state() {
        let state = state(Some(Path::new(r"D:\VibeDesk\VibeDeskProjects")));
        println!(
            "{}",
            serde_json::to_string_pretty(&state).unwrap_or_default()
        );
    }

    /* -------------------------------------------------------------- *
     * The graph
     * -------------------------------------------------------------- */

    /// A real page, trimmed to two rows.
    ///
    /// The server caps a page by output budget rather than by row count, so
    /// `has_more` and `next_cursor` are the normal case and not an edge case.
    const NODE_PAGE: &str = r#"{"columns":["qn","labels","name","file","line","complexity","importance","entry","exported","test","lines","degree"],"rows":[["D-VibeDesk-VibeDeskProjects-LivingMiniture.src.main.js.__file__","[\"File\"]","main.js","src/main.js","0","","","","","","","84"],["D-VibeDesk-VibeDeskProjects-LivingMiniture.src.config","[\"Module\"]","src/config.js","src/config.js","1","0","","false","true","false","0","66"]],"returned":2,"total":266,"total_relation":"eq","has_more":true,"truncated":true,"truncation_reason":"output_budget","next_cursor":"q1.abc","next_offset":2}"#;

    const EDGE_PAGE: &str = r#"{"columns":["s","t","type"],"rows":[["A","B","CONTAINS_FILE"],["B","C","CALLS"]],"returned":2,"total":733,"total_relation":"eq","has_more":false,"truncated":false}"#;

    const SCHEMA: &str = r#"{"node_labels":[{"label":"Function","count":109},{"label":"File","count":20}],"edge_types":[{"type":"DEFINES","count":253},{"type":"CALLS","count":150}],"total":4,"returned":4,"has_more":false}"#;

    fn node(id: &str) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            name: id.to_string(),
            label: "Function".to_string(),
            file: String::new(),
            line: None,
            complexity: None,
            importance: None,
            entry: false,
            exported: false,
            test: false,
            lines: None,
            degree: 0,
        }
    }

    #[test]
    fn a_page_keeps_its_cursor_and_its_total() {
        let page = parse_query_page(NODE_PAGE).expect("a page");
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.total, 266);
        assert!(page.has_more);
        assert_eq!(page.next_cursor.as_deref(), Some("q1.abc"));
        assert_eq!(page.columns[0], "qn");
    }

    #[test]
    fn a_node_row_becomes_a_node_with_its_label_unwrapped() {
        let page = parse_query_page(NODE_PAGE).expect("a page");
        let nodes = nodes_from_page(&page);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].label, "File");
        assert_eq!(nodes[0].name, "main.js");
        assert_eq!(nodes[0].file, "src/main.js");
        assert_eq!(nodes[0].degree, 84);
        assert_eq!(nodes[1].label, "Module");
        assert!(nodes[1].exported);
        assert!(!nodes[1].entry);
    }

    #[test]
    fn an_unset_property_is_absent_not_zero() {
        let page = parse_query_page(NODE_PAGE).expect("a page");
        let nodes = nodes_from_page(&page);
        // The File row has no complexity. Reading `""` as a number would make
        // every node without one look like the simplest node in the graph.
        assert_eq!(nodes[0].complexity, None);
        assert_eq!(nodes[0].lines, None);
        // A real zero is still a zero.
        assert_eq!(nodes[1].complexity, Some(0.0));
        assert_eq!(nodes[1].lines, Some(0));
        assert_eq!(nodes[0].line, Some(0));
    }

    #[test]
    fn an_edge_row_becomes_an_edge() {
        let page = parse_query_page(EDGE_PAGE).expect("a page");
        let edges = edges_from_page(&page);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].source, "A");
        assert_eq!(edges[0].target, "B");
        assert_eq!(edges[0].kind, "CONTAINS_FILE");
        assert_eq!(edges[1].kind, "CALLS");
    }

    #[test]
    fn a_page_that_is_not_json_is_not_a_page() {
        assert!(parse_query_page("not json").is_none());
        assert!(parse_query_page(r#"{"error":"missing required argument: project"}"#).is_some());
    }

    #[test]
    fn a_page_without_the_expected_columns_yields_nothing() {
        let page = parse_query_page(r#"{"columns":["x"],"rows":[["1"]]}"#).expect("a page");
        assert!(nodes_from_page(&page).is_empty());
        assert!(edges_from_page(&page).is_empty());
    }

    #[test]
    fn the_schema_gives_exact_totals() {
        let (labels, kinds) = parse_schema(SCHEMA).expect("a schema");
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].label, "Function");
        assert_eq!(labels[0].count, 109);
        assert_eq!(kinds[0].label, "DEFINES");
        assert_eq!(kinds[0].count, 253);
        assert_eq!(labels.iter().map(|entry| entry.count).sum::<u64>(), 129);
    }

    #[test]
    fn a_label_cell_that_is_not_a_list_is_taken_as_the_label() {
        assert_eq!(first_label(r#"["Function"]"#), "Function");
        assert_eq!(first_label("Function"), "Function");
        assert_eq!(first_label(r#"["Class","Thing"]"#), "Class");
        assert_eq!(first_label(""), "");
    }

    #[test]
    fn an_edge_to_a_node_that_was_not_read_is_dropped() {
        let nodes = vec![node("A"), node("B")];
        let edges = vec![
            GraphEdge {
                source: "A".into(),
                target: "B".into(),
                kind: "CALLS".into(),
            },
            GraphEdge {
                source: "A".into(),
                target: "Z".into(),
                kind: "CALLS".into(),
            },
        ];
        let kept = prune_edges(&nodes, edges);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, "B");
    }

    /// The wire shape the renderer reads.
    ///
    /// A rename that only lands on one side of the boundary does not fail to
    /// compile and does not throw — it draws an empty canvas. This pins the
    /// keys the TypeScript interface declares.
    #[test]
    fn the_graph_serialises_to_the_shape_the_renderer_reads() {
        let graph = CodeGraphGraph {
            project: "p".into(),
            nodes: vec![node("A")],
            edges: vec![GraphEdge {
                source: "A".into(),
                target: "A".into(),
                kind: "CALLS".into(),
            }],
            labels: vec![LabelCount {
                label: "Function".into(),
                count: 1,
            }],
            kinds: vec![LabelCount {
                label: "CALLS".into(),
                count: 1,
            }],
            total_nodes: 1,
            total_edges: 1,
            truncated: false,
            truncation: None,
        };

        let value = serde_json::to_value(&graph).expect("json");
        assert_eq!(value["totalNodes"], 1);
        assert_eq!(value["totalEdges"], 1);
        assert_eq!(value["nodes"][0]["id"], "A");
        assert_eq!(value["nodes"][0]["label"], "Function");
        assert_eq!(value["nodes"][0]["degree"], 0);
        assert_eq!(value["edges"][0]["kind"], "CALLS");
        assert_eq!(value["labels"][0]["label"], "Function");
        assert_eq!(value["kinds"][0]["count"], 1);
        assert!(value["truncation"].is_null());
    }

    /// Diagnostic, not an assertion: the real graph of this machine's project.
    #[test]
    #[ignore]
    fn print_the_code_graph() {        let graph = graph(
            "D-VibeDesk-VibeDeskProjects-LivingMiniture",
            Some(Path::new(r"D:\VibeDesk\VibeDeskProjects\LivingMiniture")),
        )
        .expect("a graph");
        println!(
            "nodes {} edges {} total {} / {} truncated {}",
            graph.nodes.len(),
            graph.edges.len(),
            graph.total_nodes,
            graph.total_edges,
            graph.truncated
        );
        println!("labels {:?}", graph.labels);
        println!("kinds {:?}", graph.kinds);
        println!(
            "first node {:?}",
            graph.nodes.first().map(|node| (&node.name, &node.label, node.degree))
        );
    }
}
