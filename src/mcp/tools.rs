use crate::graph::blast::{
    calculate_blast_radius, calculate_downstream_blast, format_sink_hits, parse_sink_list,
    sweep_literal_sinks,
};
use crate::graph::doctor::MAX_DOCTOR_LIMIT;
use crate::graph::map::{
    compute_personalized_scores, generate_map_with_scores, validate_map_scope,
};
use crate::graph::{
    format_downstream, format_upstream, format_uses, slice_line_coordinate, zero_caller_hint,
    Resolution, SymbolGraph,
};
use crate::mcp::protocol::ToolError;
use crate::memory::{MemoryLedger, MAX_DEBT_CEILING};
use crate::model::Coordinate;
use crate::workspace::walker::{
    confine_to_workspace, is_disallowed_workspace_root, WorkspaceScope,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: String) -> Self {
        Self {
            text,
            is_error: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpSession {
    pub root: PathBuf,
}

impl McpSession {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

pub fn confine(session_root: &Path, candidate: &Path) -> Result<PathBuf, ToolError> {
    confine_to_workspace(session_root, candidate)
        .map_err(|e| ToolError::InvalidParams(e.to_string()))
}

const MAX_CACHED_WORKSPACES: usize = 8;

struct CachedEntry {
    stat_fp: String,
    content_fp: String,
    graph: Arc<SymbolGraph>,
}

/// Stat-gated, content-hash-verified LRU cache for warm MCP sessions.
pub struct McpCache {
    graphs: HashMap<PathBuf, CachedEntry>,
    lru: VecDeque<PathBuf>,
}

impl Default for McpCache {
    fn default() -> Self {
        Self::new()
    }
}

impl McpCache {
    pub fn new() -> Self {
        Self {
            graphs: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn touch_lru(&mut self, key: &Path) {
        if let Some(pos) = self.lru.iter().position(|p| p == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key.to_path_buf());
    }

    pub fn get_graph(&mut self, root: &Path) -> anyhow::Result<Arc<SymbolGraph>> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let stat_fp = crate::storage::quick_stat_fingerprint(&canonical_root)?;

        // Fast path: POSIX ino+size+mtime+ctime unchanged -> zero file content reads.
        if let Some(entry) = self.graphs.get(&canonical_root) {
            if entry.stat_fp == stat_fp {
                let g = Arc::clone(&entry.graph);
                self.touch_lru(&canonical_root);
                return Ok(g);
            }
        }

        // Metadata changed or cold miss: verify strict FNV-1a content fingerprint.
        let prep = crate::storage::prepare_workspace(&canonical_root)?;
        if let Some(entry) = self.graphs.get_mut(&canonical_root) {
            if entry.content_fp == prep.content_fingerprint {
                entry.stat_fp = stat_fp;
                let g = Arc::clone(&entry.graph);
                self.touch_lru(&canonical_root);
                return Ok(g);
            }
        }

        let graph = crate::storage::sync_prepared_workspace(&canonical_root, &prep)?;
        let arc_graph = Arc::new(graph);

        if !self.graphs.contains_key(&canonical_root) {
            while self.graphs.len() >= MAX_CACHED_WORKSPACES {
                if let Some(evicted) = self.lru.pop_front() {
                    self.graphs.remove(&evicted);
                } else {
                    break;
                }
            }
        }

        self.graphs.insert(
            canonical_root.clone(),
            CachedEntry {
                stat_fp,
                content_fp: prep.content_fingerprint,
                graph: Arc::clone(&arc_graph),
            },
        );
        self.touch_lru(&canonical_root);
        Ok(arc_graph)
    }
}

fn string_or_array_schema(description: &str) -> Value {
    json!({
        "oneOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "string" } }
        ],
        "description": description
    })
}

pub fn list_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "mimori_slice",
            "description": "Extract token-dense AST slice: source body, coordinates, signature, scoped memory notes, and 1-hop callers/callees. Bodies >250 lines are head/tail truncated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "coordinate": string_or_array_schema("Target symbol(s) or coordinate(s). Supports comma-separated batching or string[] (e.g. 'a.ts:foo, b.ts:bar, c.ts:#L20-45' or bare 'symbol')"),
                    "budget": {
                        "type": "integer",
                        "description": "Target token budget to pack markdown output (strictly honored whenever core fits)"
                    },
                    "follow_local": {
                        "type": "boolean",
                        "description": "Inline private local callee bodies declared within the same file"
                    },
                    "with_imports": {
                        "type": "boolean",
                        "description": "Include top-of-file import statements in slice header"
                    },
                    "numbered": {
                        "type": "boolean",
                        "description": "Prefix body lines with 1-based line coordinates (L{n}: ...)"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["coordinate"]
            }
        }),
        json!({
            "name": "mimori_dump",
            "description": "Generate Turn-0 packed context snapshot: module-stratified architectural map, ADRs, domain vocabulary, scoped gotchas, and active debt under token budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "budget": {
                        "type": "integer",
                        "description": "Token budget for context snapshot (default: 1500 tokens)"
                    },
                    "focus": {
                        "type": "string",
                        "description": "Optional focus symbol or coordinate to personalize the architectural map"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                }
            }
        }),
        json!({
            "name": "mimori_map",
            "description": "Generate module-stratified, centrality-ranked structural outline of codebase symbols, modules, and entry points (defaults to top 30; pass limit: 0 for all).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of symbols to include (default: 30; 0 = unlimited)"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Restrict map to files within this subdirectory"
                    },
                    "seed": {
                        "type": "string",
                        "description": "Bias ranking toward symbols whose name or file matches this token"
                    },
                    "focus": {
                        "type": "string",
                        "description": "Run Personalized PageRank (PPR) biased toward this target symbol"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                }
            }
        }),
        json!({
            "name": "mimori_find",
            "description": "Search for symbols and files across the repository, ordered by exact match and PageRank centrality.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Symbol name, path search term, or literal fallback string. Supports pipe/comma-separated multi-query batching in one call (e.g. 'foo|bar|baz')"
                    },
                    "symbols_only": {
                        "type": "boolean",
                        "description": "Restrict search strictly to symbol declarations"
                    },
                    "files_only": {
                        "type": "boolean",
                        "description": "Restrict search strictly to file paths"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Optional directory or file path prefix filter"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 50)"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "mimori_blast",
            "description": "Inspect blast radius: upstream callers, affected entry points, unresolved member warnings, scoped memory, and downstream sinks.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": string_or_array_schema("Target symbol(s) or coordinate(s). Supports comma-separated batching or string[]"),
                    "depth": {
                        "type": "integer",
                        "description": "Traversal depth (default 3)"
                    },
                    "down": {
                        "type": "boolean",
                        "description": "Traverse downstream callees instead of upstream callers"
                    },
                    "with_sinks": {
                        "type": "string",
                        "description": "Comma-separated literal sink substrings to sweep within the blast cone"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["target"]
            }
        }),
        json!({
            "name": "mimori_graph",
            "description": "Traverse symbol relationships: upstream callers ('up'), downstream callees ('down'), or mentioners ('uses').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": string_or_array_schema("Target coordinate(s) or symbol(s). Supports comma-separated batching or string[]"),
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down", "uses"],
                        "description": "Traversal direction: 'up' (callers), 'down' (callees), 'uses' (property/arg/template mentions)"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["target", "direction"]
            }
        }),
        json!({
            "name": "mimori_memory",
            "description": "Read, add, lint, or resolve project memory (.agents/memory.md) with JIT @scope:/@sym: anchors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["show", "add", "lint", "resolve"],
                        "description": "Action: 'show', 'add' (append structured/scoped entry), 'lint', or 'resolve' (delete matched debt item)"
                    },
                    "section": {
                        "type": "string",
                        "description": "Section for 'show' or 'add': 'epics', 'debt', 'vocab', 'gotchas'"
                    },
                    "target": {
                        "type": "string",
                        "description": "Pattern matching debt line to delete (for 'resolve')"
                    },
                    "text": {
                        "type": "string",
                        "description": "Single-line entry text (for 'add')"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Optional @scope: file/dir glob anchor (for 'add')"
                    },
                    "sym": {
                        "type": "string",
                        "description": "Optional @sym: symbol anchor (for 'add')"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Delete all matches when multiple debt lines match (for 'resolve')"
                    },
                    "budget": {
                        "type": "integer",
                        "description": "Token budget for output"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mimori_debt",
            "description": "Scan, check, or sync in-code mimodept technical debt markers extracted via Tree-sitter comment ASTs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "check", "sync"],
                        "description": "Action: 'list' (scan in-code markers), 'check' (validate syntax & readability), 'sync' (merge into .agents/memory.md)"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Optional subdirectory filter"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "mimori_diff",
            "description": "Inspect git diff hunk spans intersected with AST symbols: mutated symbols, combined upstream blast cone, scoped memory, affected test suites, and debt/memory gate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "staged": {
                        "type": "boolean",
                        "description": "Inspect staged changes (--staged) instead of working tree"
                    },
                    "base": {
                        "type": "string",
                        "description": "Optional base git revision to diff against (e.g. HEAD~1 or main)"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Maximum traversal depth for upstream callers (default 3)"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                }
            }
        }),
        json!({
            "name": "mimori_missing",
            "description": "Absence query: list source files under a scope whose content lacks a literal pattern (supports '|' alternatives and optional 'defines' pre-filter).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Literal substring to look for; '|' separates alternatives (e.g. 'Remote|serverId')"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Workspace-relative directory to sweep"
                    },
                    "defines": {
                        "type": "string",
                        "description": "Only consider files whose content contains this marker"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "mimori_doctor",
            "description": "Repository health audit: file/symbol/edge counts, resolution stats, top fan-in hubs, and dead-weight candidates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Keep only the top N candidates per tier (capped at 500)"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory relative to session root"
                    }
                }
            }
        }),
    ]
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

fn split_target_tokens(raw: &str) -> Vec<String> {
    let items: Vec<String> = raw
        .split([',', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        vec![raw.to_string()]
    } else {
        items
    }
}

fn deserialize_targets<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Single(s) => Ok(split_target_tokens(&s)),
        StringOrVec::Multiple(list) => {
            let cleaned: Vec<String> = list
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if cleaned.is_empty() {
                return Err(serde::de::Error::custom("target list must not be empty"));
            }
            Ok(cleaned)
        }
    }
}

#[derive(Deserialize)]
struct SliceToolArgs {
    #[serde(alias = "coordinates", deserialize_with = "deserialize_targets")]
    coordinate: Vec<String>,
    #[serde(default)]
    budget: Option<usize>,
    #[serde(default)]
    follow_local: Option<bool>,
    #[serde(default)]
    with_imports: Option<bool>,
    #[serde(default)]
    numbered: Option<bool>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct DumpToolArgs {
    #[serde(default)]
    budget: Option<usize>,
    #[serde(default)]
    focus: Option<String>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct MapToolArgs {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    seed: Option<String>,
    #[serde(default)]
    focus: Option<String>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct FindToolArgs {
    pattern: String,
    #[serde(default)]
    symbols_only: Option<bool>,
    #[serde(default)]
    files_only: Option<bool>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct DiffToolArgs {
    #[serde(default)]
    staged: Option<bool>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct BlastToolArgs {
    #[serde(alias = "targets", deserialize_with = "deserialize_targets")]
    target: Vec<String>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    down: Option<bool>,
    #[serde(default)]
    with_sinks: Option<String>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct GraphToolArgs {
    #[serde(alias = "targets", deserialize_with = "deserialize_targets")]
    target: Vec<String>,
    direction: String,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct MemoryToolArgs {
    action: String,
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    sym: Option<String>,
    #[serde(default)]
    all: Option<bool>,
    #[serde(default)]
    budget: Option<usize>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct DebtToolArgs {
    action: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct MissingToolArgs {
    pattern: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    defines: Option<String>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct DoctorToolArgs {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

fn parse_args<T: for<'de> Deserialize<'de>>(
    tool_name: &str,
    arguments: &Value,
) -> Result<T, ToolError> {
    serde_json::from_value(arguments.clone()).map_err(|e| {
        ToolError::InvalidParams(format!("Invalid arguments for '{}': {}", tool_name, e))
    })
}

fn resolve_workspace_scope(
    workspace_dir: Option<&str>,
    session_root: &Path,
) -> Result<PathBuf, ToolError> {
    let resolved = match workspace_dir {
        Some(ws) => {
            let path = Path::new(ws);
            if path.is_absolute() {
                let canon_root = session_root
                    .canonicalize()
                    .unwrap_or_else(|_| session_root.to_path_buf());
                if let Ok(rel) = path
                    .strip_prefix(session_root)
                    .or_else(|_| path.strip_prefix(&canon_root))
                {
                    confine(session_root, &session_root.join(rel))?
                } else {
                    return Err(ToolError::InvalidParams(format!(
                        "workspace_dir must be relative to the session root, got absolute path: {}",
                        ws
                    )));
                }
            } else {
                confine(session_root, path)?
            }
        }
        None => session_root.to_path_buf(),
    };

    if is_disallowed_workspace_root(&resolved) {
        return Err(ToolError::Execution(format!(
            "Refusing to operate on home or system directory '{}' as workspace",
            resolved.display()
        )));
    }

    Ok(resolved)
}

/// If `scope_dir` is a plain subdirectory of `session.root` (no own `.git` or
/// `.mimori`), reuse `session.root`'s cached graph plus a relative prefix so
/// subdir calls never create stray `<subdir>/.mimori/index.db` caches.
fn resolve_graph_root_and_prefix(
    scope_dir: &Path,
    session_root: &Path,
) -> (PathBuf, Option<String>) {
    let canon_session = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.to_path_buf());
    let canon_scope = scope_dir
        .canonicalize()
        .unwrap_or_else(|_| scope_dir.to_path_buf());

    if canon_scope == canon_session
        || canon_scope.join(".git").exists()
        || canon_scope.join(".mimori").exists()
    {
        return (canon_scope, None);
    }

    if let Ok(rel) = canon_scope.strip_prefix(&canon_session) {
        let norm = crate::workspace::walker::normalize_rel_str(rel);
        if !norm.is_empty() {
            return (canon_session, Some(norm));
        }
    }
    (canon_scope, None)
}

fn combine_scope_prefix(base_prefix: Option<&str>, extra_scope: Option<&str>) -> Option<String> {
    match (base_prefix, extra_scope) {
        (None, None) => None,
        (Some(b), None) => Some(b.to_string()),
        (None, Some(e)) => Some(e.to_string()),
        (Some(b), Some(e)) => {
            let clean = e.trim().trim_start_matches("./");
            if clean.is_empty() || clean == "." {
                Some(b.to_string())
            } else {
                Some(format!("{}/{}", b.trim_end_matches('/'), clean))
            }
        }
    }
}

fn scope_coord_for_graph(coord: Coordinate, scope_dir: &Path, graph_root: &Path) -> Coordinate {
    let Some(file) = coord.file() else {
        return coord;
    };
    let candidate = if file.is_absolute() {
        file.to_path_buf()
    } else if scope_dir != graph_root {
        scope_dir.join(file)
    } else {
        graph_root.join(file)
    };
    if let Ok(confined) = confine_to_workspace(graph_root, &candidate) {
        if let Ok(rel) = confined.strip_prefix(graph_root) {
            let rel_buf = rel.to_path_buf();
            return match coord {
                Coordinate::Symbol { name, .. } => Coordinate::Symbol {
                    file: rel_buf,
                    name,
                },
                Coordinate::Lines { start, end, .. } => Coordinate::Lines {
                    file: rel_buf,
                    start,
                    end,
                },
                other => other,
            };
        }
    }
    coord.normalize_against(graph_root)
}

pub fn call_tool(
    name: &str,
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    match name {
        "mimori_slice" => handle_slice(arguments, session, cache),
        "mimori_map" => handle_map(arguments, session, cache),
        "mimori_find" => handle_find(arguments, session, cache),
        "mimori_diff" => handle_diff(arguments, session, cache),
        "mimori_blast" => handle_blast(arguments, session, cache),
        "mimori_graph" => handle_graph(arguments, session, cache),
        "mimori_memory" => handle_memory(arguments, session),
        "mimori_debt" => handle_debt(arguments, session),
        "mimori_dump" => handle_dump(arguments, session, cache),
        "mimori_missing" => handle_missing(arguments, session),
        "mimori_doctor" => handle_doctor(arguments, session, cache),
        unknown => Err(ToolError::NotFound(unknown.to_string())),
    }
}

fn handle_slice(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: SliceToolArgs = parse_args("mimori_slice", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, _) = resolve_graph_root_and_prefix(&scope_dir, &session.root);

    let with_imports = args.with_imports.unwrap_or(false);
    let follow_local = args.follow_local.unwrap_or(false);
    let numbered = args.numbered.unwrap_or(false);
    let per_budget = args.budget.map(|b| b / args.coordinate.len().max(1));

    let mut cached_graph: Option<Arc<SymbolGraph>> = None;
    let mut rendered = Vec::with_capacity(args.coordinate.len());
    for raw_coord in &args.coordinate {
        let coord = Coordinate::parse(raw_coord).map_err(|e| {
            ToolError::InvalidParams(format!("Invalid coordinate '{}': {}", raw_coord, e))
        })?;

        let slice = if let Coordinate::Lines { file, start, end } = &coord {
            let confined = confine(&scope_dir, file)?;
            slice_line_coordinate(&confined, *start, *end, with_imports, Some(&scope_dir))
                .map_err(|e| ToolError::Execution(e.to_string()))?
        } else {
            let graph = match &cached_graph {
                Some(g) => Arc::clone(g),
                None => {
                    let g = cache
                        .get_graph(&graph_root)
                        .map_err(|e| ToolError::Execution(e.to_string()))?;
                    cached_graph = Some(Arc::clone(&g));
                    g
                }
            };
            let norm_coord = scope_coord_for_graph(coord, &scope_dir, &graph_root);
            graph
                .build_slice(&norm_coord, follow_local, with_imports)
                .map_err(|e| ToolError::Execution(e.to_string()))?
        };

        if let Some(b) = per_budget {
            rendered.push(slice.render_markdown_budgeted(b, numbered));
        } else {
            rendered.push(slice.render_markdown(numbered));
        }
    }
    Ok(ToolOutcome::ok(rendered.join("\n---\n")))
}

fn handle_map(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: MapToolArgs = parse_args("mimori_map", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, sub_prefix) = resolve_graph_root_and_prefix(&scope_dir, &session.root);

    let effective_raw_scope = combine_scope_prefix(sub_prefix.as_deref(), args.scope.as_deref());
    let validated_scope = validate_map_scope(&graph_root, effective_raw_scope.as_deref())
        .map_err(|e| ToolError::InvalidParams(e.to_string()))?;

    let base_graph = cache
        .get_graph(&graph_root)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let personalized = compute_personalized_scores(
        &base_graph,
        args.focus.as_deref(),
        args.seed.as_deref(),
        &scope_dir,
    )
    .map_err(|e| ToolError::Execution(e.to_string()))?;
    let scores_ref = personalized
        .as_ref()
        .map(|(scores, stats)| (scores.as_slice(), stats));

    let map_result = generate_map_with_scores(
        &base_graph,
        scores_ref,
        validated_scope.as_deref(),
        args.focus.as_deref(),
        args.limit,
    );
    Ok(ToolOutcome::ok(map_result.to_markdown()))
}

fn handle_find(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: FindToolArgs = parse_args("mimori_find", arguments)?;
    if args.pattern.trim().is_empty() {
        return Err(ToolError::InvalidParams(
            "Find pattern cannot be empty".to_string(),
        ));
    }
    let symbols_only = args.symbols_only.unwrap_or(false);
    let files_only = args.files_only.unwrap_or(false);
    if symbols_only && files_only {
        return Err(ToolError::InvalidParams(
            "Cannot combine symbols_only and files_only".to_string(),
        ));
    }

    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, sub_prefix) = resolve_graph_root_and_prefix(&scope_dir, &session.root);
    let effective_scope = combine_scope_prefix(sub_prefix.as_deref(), args.scope.as_deref());
    if let Some(sc) = effective_scope.as_deref() {
        WorkspaceScope::parse(&graph_root, sc, false)
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    }

    let graph = cache
        .get_graph(&graph_root)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let limit = args.limit.or(Some(50));
    let res = crate::workspace::execute_find_with_graph(
        &graph_root,
        &graph,
        &args.pattern,
        symbols_only,
        files_only,
        effective_scope.as_deref(),
        limit,
    )
    .map_err(|e| ToolError::Execution(e.to_string()))?;
    Ok(ToolOutcome::ok(res.to_markdown()))
}

fn handle_diff(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: DiffToolArgs = parse_args("mimori_diff", arguments)?;
    crate::graph::diff::validate_diff_base(args.base.as_deref())
        .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let graph = cache
        .get_graph(&scope_dir)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let report = crate::graph::diff::calculate_diff_blast(
        &scope_dir,
        &graph,
        args.staged.unwrap_or(false),
        args.base.as_deref(),
        args.depth.unwrap_or(3),
    )
    .map_err(|e| ToolError::Execution(e.to_string()))?;
    Ok(ToolOutcome::ok(report.to_markdown()))
}

fn handle_blast(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: BlastToolArgs = parse_args("mimori_blast", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, _) = resolve_graph_root_and_prefix(&scope_dir, &session.root);
    let graph = cache
        .get_graph(&graph_root)
        .map_err(|e| ToolError::Execution(e.to_string()))?;

    let depth = args.depth.unwrap_or(3);
    let down = args.down.unwrap_or(false);
    let sinks = parse_sink_list(args.with_sinks.as_deref());

    let mut blocks = Vec::with_capacity(args.target.len());
    for raw_target in &args.target {
        let parsed =
            Coordinate::parse(raw_target).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let coord = scope_coord_for_graph(parsed, &scope_dir, &graph_root);

        let blast_res = if down {
            calculate_downstream_blast(&graph, &coord, depth)
                .map_err(|e| ToolError::Execution(e.to_string()))?
        } else {
            calculate_blast_radius(&graph, &coord, depth)
                .map_err(|e| ToolError::Execution(e.to_string()))?
        };

        let mut md = blast_res.to_markdown();
        if blast_res.affected.is_empty() && blast_res.value_uses.is_empty() && !down {
            if let Some(hint) = zero_caller_hint(&graph, &coord) {
                md.push('\n');
                md.push_str(&hint);
                md.push('\n');
            }
        }

        if !sinks.is_empty() {
            let target_file = match graph.resolve(&coord) {
                Resolution::Unique(i) => Some(graph.symbols[i].file.clone()),
                Resolution::Ambiguous(v) => v.first().map(|&i| graph.symbols[i].file.clone()),
                Resolution::NotFound => None,
            };
            let cone = blast_res.cone_files(target_file.as_deref());
            let (sink_hits, truncated) =
                sweep_literal_sinks(&graph_root, &graph, &sinks, Some(&cone));
            md.push_str(&format_sink_hits(&sinks, &sink_hits, truncated));
        } else {
            md.push_str("\nTip: blast follows call edges plus labeled value-use rows — raw literals (e.g. console.*, logPath) need a sweep: pass with_sinks or use rg.\n");
        }
        blocks.push(md);
    }

    Ok(ToolOutcome::ok(blocks.join("\n---\n")))
}

fn handle_graph(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: GraphToolArgs = parse_args("mimori_graph", arguments)?;
    if !matches!(args.direction.as_str(), "up" | "down" | "uses") {
        return Err(ToolError::InvalidParams(format!(
            "Invalid direction '{}': expected 'up', 'down', or 'uses'",
            args.direction
        )));
    }
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, _) = resolve_graph_root_and_prefix(&scope_dir, &session.root);
    let graph = cache
        .get_graph(&graph_root)
        .map_err(|e| ToolError::Execution(e.to_string()))?;

    let mut blocks = Vec::with_capacity(args.target.len());
    for raw_target in &args.target {
        let parsed =
            Coordinate::parse(raw_target).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let coord = scope_coord_for_graph(parsed, &scope_dir, &graph_root);

        let block = match args.direction.as_str() {
            "up" => {
                let (callers, value_uses) = graph
                    .upstream_strict(&coord)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                format_upstream(&graph, &coord, &callers, &value_uses)
            }
            "down" => {
                let callees = graph
                    .callees_strict(&coord)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                format_downstream(&graph, &coord, &callees)
            }
            "uses" => {
                let mentioners = graph
                    .mentioners_strict(&coord)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                format_uses(&graph, &coord, &mentioners)
            }
            _ => unreachable!(),
        };
        blocks.push(block);
    }
    Ok(ToolOutcome::ok(blocks.join("\n---\n")))
}

fn handle_memory(arguments: &Value, session: &McpSession) -> Result<ToolOutcome, ToolError> {
    let args: MemoryToolArgs = parse_args("mimori_memory", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;

    match args.action.as_str() {
        "show" => {
            let ledger = MemoryLedger::load(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Load memory: {}", e)))?;
            let text = if let Some(sec) = args.section.as_deref() {
                match ledger.get_section(sec) {
                    Some(content) => content,
                    None => format!("MEM_EMPTY: section '{}' not found; exit 0.", sec),
                }
            } else {
                ledger.raw_content
            };
            if let Some(b) = args.budget {
                let max_chars = b.saturating_mul(4);
                if text.chars().count() > max_chars {
                    let mut truncated = String::new();
                    let mut used = 0usize;
                    for line in text.lines() {
                        let line_chars = line.chars().count() + 1;
                        if used.saturating_add(line_chars) > max_chars {
                            truncated.push_str("... [truncated to fit token budget]\n");
                            break;
                        }
                        truncated.push_str(line);
                        truncated.push('\n');
                        used = used.saturating_add(line_chars);
                    }
                    return Ok(ToolOutcome::ok(truncated));
                }
            }
            Ok(ToolOutcome::ok(text))
        }
        "add" => {
            let text = args.text.ok_or_else(|| {
                ToolError::InvalidParams(
                    "Missing required 'text' parameter for action 'add'".to_string(),
                )
            })?;
            let section = args.section.as_deref().unwrap_or("gotchas");
            let mem_path = MemoryLedger::memory_path(&scope_dir);
            if !mem_path.exists() {
                MemoryLedger::scaffold(&scope_dir, false)
                    .map_err(|e| ToolError::Execution(format!("Scaffold memory: {}", e)))?;
            }
            let mut ledger = MemoryLedger::load(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Load memory: {}", e)))?;
            let entry = ledger
                .add_entry(section, &text, args.scope.as_deref(), args.sym.as_deref())
                .map_err(|e| ToolError::Execution(format!("Add memory: {}", e)))?;
            Ok(ToolOutcome::ok(format!(
                "MEM_ADD: appended to '{}': {}; exit 0.",
                section, entry
            )))
        }
        "lint" | "check" => {
            let ledger = MemoryLedger::load(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Load memory: {}", e)))?;
            let report = ledger.lint();
            Ok(ToolOutcome {
                text: report.to_m2m_output(),
                is_error: !report.passed,
            })
        }
        "resolve" => {
            let target = args.target.ok_or_else(|| {
                ToolError::InvalidParams(
                    "Missing required 'target' parameter for action 'resolve'".to_string(),
                )
            })?;
            let mut ledger = MemoryLedger::load(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Load memory: {}", e)))?;
            let deleted = ledger
                .resolve(&target, args.all.unwrap_or(false))
                .map_err(|e| ToolError::Execution(format!("Resolve: {}", e)))?;
            Ok(ToolOutcome::ok(format!(
                "MEM_RESOLVE: deleted {} lines matching '{}'; debt: {}/{} lines; exit 0.\n{}",
                deleted.len(),
                target,
                ledger.raw_debt_lines.len(),
                MAX_DEBT_CEILING,
                deleted
                    .iter()
                    .map(|l| format!("  removed: {}", l))
                    .collect::<Vec<_>>()
                    .join("\n")
            )))
        }
        other => Err(ToolError::InvalidParams(format!(
            "Invalid action '{}': expected 'show', 'add', 'lint', or 'resolve'",
            other
        ))),
    }
}

fn handle_debt(arguments: &Value, session: &McpSession) -> Result<ToolOutcome, ToolError> {
    let args: DebtToolArgs = parse_args("mimori_debt", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    if let Some(s) = args.scope.as_deref() {
        WorkspaceScope::parse(&scope_dir, s, true)
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    }

    match args.action.as_str() {
        "list" => {
            let (_markers, output) = crate::memory::list_debt(&scope_dir, args.scope.as_deref())
                .map_err(|e| ToolError::Execution(e.to_string()))?;
            Ok(ToolOutcome::ok(output))
        }
        "check" => {
            let (passed, output) = crate::memory::check_debt(&scope_dir);
            Ok(ToolOutcome {
                text: output,
                is_error: !passed,
            })
        }
        "sync" => {
            let (_in_code, _manual, _synced, output) = crate::memory::sync_debt(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Debt sync: {}", e)))?;
            Ok(ToolOutcome::ok(output))
        }
        other => Err(ToolError::InvalidParams(format!(
            "Invalid action '{}': expected 'list', 'check', or 'sync'",
            other
        ))),
    }
}

fn handle_dump(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: DumpToolArgs = parse_args("mimori_dump", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let budget = args.budget.unwrap_or(1500);
    let graph = cache
        .get_graph(&scope_dir)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let dump = crate::memory::dump::generate_dump_with_graph(
        &scope_dir,
        Some(graph.as_ref()),
        budget,
        args.focus.as_deref(),
    )
    .map_err(|e| ToolError::Execution(e.to_string()))?;
    Ok(ToolOutcome::ok(dump.markdown))
}

fn handle_missing(arguments: &Value, session: &McpSession) -> Result<ToolOutcome, ToolError> {
    let args: MissingToolArgs = parse_args("mimori_missing", arguments)?;
    if crate::graph::missing::split_alternatives(&args.pattern).is_empty() {
        return Err(ToolError::InvalidParams(
            "pattern must contain at least one non-empty substring".to_string(),
        ));
    }
    if let Some(d) = args.defines.as_deref() {
        if d.trim().is_empty() {
            return Err(ToolError::InvalidParams(
                "defines must not be empty".to_string(),
            ));
        }
    }
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    if let Some(sc) = args.scope.as_deref() {
        WorkspaceScope::parse(&scope_dir, sc, true)
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    }

    let missing = crate::graph::missing::find_missing(
        &scope_dir,
        args.scope.as_deref(),
        &args.pattern,
        args.defines.as_deref(),
    )
    .map_err(|e| ToolError::Execution(e.to_string()))?;
    Ok(ToolOutcome::ok(
        crate::graph::missing::format_missing_markdown(
            &args.pattern,
            args.scope.as_deref(),
            args.defines.as_deref(),
            &missing,
        ),
    ))
}

fn handle_doctor(
    arguments: &Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<ToolOutcome, ToolError> {
    let args: DoctorToolArgs = parse_args("mimori_doctor", arguments)?;
    let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
    let (graph_root, _) = resolve_graph_root_and_prefix(&scope_dir, &session.root);
    let graph = cache
        .get_graph(&graph_root)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let capped_limit = args.limit.map(|l| l.min(MAX_DOCTOR_LIMIT));
    let report = crate::graph::doctor::run_doctor(&graph, capped_limit);
    Ok(ToolOutcome::ok(report.to_markdown()))
}
