use crate::graph::blast::{
    calculate_blast_radius, calculate_downstream_blast, format_sink_hits, parse_sink_list,
    sweep_literal_sinks,
};
use crate::graph::map::{generate_map, personalize_map};
use crate::graph::{
    format_downstream, format_upstream, format_uses, slice_line_coordinate, zero_caller_hint,
    SymbolGraph,
};
use crate::mcp::protocol::ToolError;
use crate::model::Coordinate;
use serde::Deserialize;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    let canon_root = session_root.canonicalize().map_err(|e| {
        ToolError::InvalidParams(format!(
            "Invalid workspace root '{}': {}",
            session_root.display(),
            e
        ))
    })?;

    let canon_candidate = match candidate.canonicalize() {
        Ok(c) => c,
        Err(e) => {
            if candidate.is_absolute() && !candidate.starts_with(&canon_root) {
                return Err(ToolError::InvalidParams(format!(
                    "path escapes workspace: {}",
                    candidate.display()
                )));
            }
            return Err(ToolError::InvalidParams(format!(
                "Cannot resolve path '{}': {}",
                candidate.display(),
                e
            )));
        }
    };

    if canon_candidate.starts_with(&canon_root) {
        Ok(canon_candidate)
    } else {
        Err(ToolError::InvalidParams(format!(
            "path escapes workspace: {}",
            candidate.display()
        )))
    }
}

pub struct McpCache {
    graphs: HashMap<PathBuf, (String, Arc<SymbolGraph>)>,
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
        }
    }

    pub fn get_graph(&mut self, root: &Path) -> anyhow::Result<Arc<SymbolGraph>> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let current_fingerprint = compute_fingerprint(&canonical_root);

        if let Some((fp, graph)) = self.graphs.get(&canonical_root) {
            if *fp == current_fingerprint {
                return Ok(Arc::clone(graph));
            }
        }

        let graph = crate::storage::get_or_sync_graph(&canonical_root)?;
        let arc_graph = Arc::new(graph);
        self.graphs.insert(
            canonical_root,
            (current_fingerprint, Arc::clone(&arc_graph)),
        );
        Ok(arc_graph)
    }
}

fn compute_fingerprint(root: &Path) -> String {
    let aliases = crate::workspace::AliasSet::collect(root);
    let (scans, _) = crate::workspace::scan_workspace_with_stats(root);

    let mut hasher = DefaultHasher::new();
    aliases.fingerprint().hash(&mut hasher);
    for scan in scans {
        scan.rel.hash(&mut hasher);
        scan.hash.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub fn list_tools() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "mimori_slice",
            "description": "Extract token-dense AST slice: source body, coordinates, signature, and 1-hop callers/callees. Bodies >250 lines are head/tail truncated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "coordinate": {
                        "type": "string",
                        "description": "Target symbol or coordinate (e.g. 'path/file.ts:symbol', 'path/file.ts:#L20-45', or bare 'symbol')"
                    },
                    "budget": {
                        "type": "integer",
                        "description": "Target token budget to pack markdown output (heuristically packs imports/callees/callers)"
                    },
                    "follow_local": {
                        "type": "boolean",
                        "description": "Inline private local callee bodies declared within the same file"
                    },
                    "with_imports": {
                        "type": "boolean",
                        "description": "Include top-of-file import statements in slice header"
                    },
                    "workspace_dir": {
                        "type": "string",
                        "description": "Optional workspace directory (defaults to current working directory)"
                    }
                },
                "required": ["coordinate"]
            }
        }),
        json!({
            "name": "mimori_map",
            "description": "Generate centrality-ranked PageRank structural outline of codebase symbols, modules, and entry points.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of symbols to include (recommended: 20-50 for initial orientation)"
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
                        "description": "Optional workspace directory"
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
                        "description": "Symbol name or path search term"
                    },
                    "symbols_only": {
                        "type": "boolean",
                        "description": "Restrict search strictly to symbol declarations"
                    },
                    "files_only": {
                        "type": "boolean",
                        "description": "Restrict search strictly to file paths"
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
            "description": "Inspect blast radius: upstream callers, affected entry points, and downstream sinks before changing signatures.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Symbol to compute blast radius for"
                    },
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
                        "description": "Comma-separated list of boundary sink symbols to report"
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
                    "target": {
                        "type": "string",
                        "description": "Target coordinate or symbol"
                    },
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
            "description": "Read, lint, or resolve project memory (.agents/memory.md).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["show", "lint", "resolve"],
                        "description": "Action: 'show' (read memory sections), 'lint' (validate 30-line ceiling and schema), 'resolve' (surgically delete debt item)"
                    },
                    "section": {
                        "type": "string",
                        "description": "Optional section filter for 'show': 'epics', 'debt', 'vocab', 'gotchas'"
                    },
                    "target": {
                        "type": "string",
                        "description": "Pattern matching debt line to delete (for 'resolve')"
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
            "description": "Scan, check, or sync in-code ponytail technical debt markers (# ponytail: <what> <- <ceiling> -> <trigger>).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "check", "sync"],
                        "description": "Action: 'list' (scan in-code markers), 'check' (validate in CI), 'sync' (merge into .agents/memory.md)"
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
    ]
}

#[derive(Deserialize)]
struct SliceToolArgs {
    coordinate: String,
    #[serde(default)]
    budget: Option<usize>,
    #[serde(default)]
    follow_local: Option<bool>,
    #[serde(default)]
    with_imports: Option<bool>,
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
    limit: Option<usize>,
    #[serde(default)]
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
struct BlastToolArgs {
    target: String,
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
    target: String,
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

fn resolve_workspace_scope(
    workspace_dir: Option<&str>,
    session_root: &Path,
) -> Result<PathBuf, ToolError> {
    match workspace_dir {
        Some(ws) => {
            let path = Path::new(ws);
            if path.is_absolute() {
                return Err(ToolError::InvalidParams(format!(
                    "workspace_dir must be relative to the session root, got absolute path: {}",
                    ws
                )));
            }
            let target = session_root.join(path);
            confine(session_root, &target)
        }
        None => Ok(session_root.to_path_buf()),
    }
}

pub fn call_tool(
    name: &str,
    arguments: &serde_json::Value,
    session: &McpSession,
    cache: &mut McpCache,
) -> Result<String, ToolError> {
    match name {
        "mimori_slice" => {
            let args: SliceToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_slice': {}", e))
            })?;
            let _scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let coord = Coordinate::parse(&args.coordinate).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid coordinate '{}': {}", args.coordinate, e))
            })?;

            let with_imports = args.with_imports.unwrap_or(false);
            let follow_local = args.follow_local.unwrap_or(false);

            if let Coordinate::Lines { file, start, end } = &coord {
                let full = if file.is_absolute() {
                    file.clone()
                } else {
                    session.root.join(file)
                };
                let confined = confine(&session.root, &full)?;
                let slice = slice_line_coordinate(&confined, *start, *end, with_imports)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                if let Some(b) = args.budget {
                    Ok(slice.to_markdown_budgeted(b))
                } else {
                    Ok(slice.to_markdown())
                }
            } else {
                let graph = cache
                    .get_graph(&session.root)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                let norm_coord = coord.normalize_against(&session.root);
                let slice = graph
                    .build_slice(&norm_coord, follow_local, with_imports)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                if let Some(b) = args.budget {
                    Ok(slice.to_markdown_budgeted(b))
                } else {
                    Ok(slice.to_markdown())
                }
            }
        }
        "mimori_map" => {
            let args: MapToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_map': {}", e))
            })?;
            let _scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let base_graph = cache
                .get_graph(&session.root)
                .map_err(|e| ToolError::Execution(e.to_string()))?;
            let mut graph = (*base_graph).clone();

            personalize_map(
                &mut graph,
                args.focus.as_deref(),
                args.seed.as_deref(),
                &session.root,
            )
            .map_err(|e| ToolError::Execution(e.to_string()))?;

            let scope = args.scope.as_deref().or(args.workspace_dir.as_deref());
            let map_result = generate_map(&graph, scope, args.focus.as_deref(), args.limit);
            Ok(map_result.to_markdown())
        }
        "mimori_find" => {
            let args: FindToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_find': {}", e))
            })?;
            let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let limit = args.limit.or(Some(50));
            let res = crate::workspace::execute_find(
                &scope_dir,
                &args.pattern,
                args.symbols_only.unwrap_or(false),
                args.files_only.unwrap_or(false),
                limit,
            )
            .map_err(|e| ToolError::Execution(e.to_string()))?;
            Ok(res.to_markdown())
        }
        "mimori_blast" => {
            let args: BlastToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_blast': {}", e))
            })?;
            let _scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let graph = cache
                .get_graph(&session.root)
                .map_err(|e| ToolError::Execution(e.to_string()))?;
            let coord = Coordinate::parse(&args.target)
                .map_err(|e| ToolError::InvalidParams(e.to_string()))?
                .normalize_against(&session.root);

            let depth = args.depth.unwrap_or(3);
            let down = args.down.unwrap_or(false);

            let blast_res = if down {
                calculate_downstream_blast(&graph, &coord, depth)
                    .map_err(|e| ToolError::Execution(e.to_string()))?
            } else {
                calculate_blast_radius(&graph, &coord, depth)
                    .map_err(|e| ToolError::Execution(e.to_string()))?
            };

            let sinks = parse_sink_list(args.with_sinks.as_deref());
            let sink_hits = if sinks.is_empty() {
                Vec::new()
            } else {
                sweep_literal_sinks(&session.root, &graph, &sinks)
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
                md.push_str(&format_sink_hits(&sinks, &sink_hits));
            } else {
                md.push_str("\nTip: blast follows call edges plus labeled value-use rows — raw literals (console.*, logPath, send*Notifications, getDetailedMessage) need a sweep: pass with_sinks or use rg.\n");
            }

            Ok(md)
        }
        "mimori_graph" => {
            let args: GraphToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_graph': {}", e))
            })?;
            let _scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let graph = cache
                .get_graph(&session.root)
                .map_err(|e| ToolError::Execution(e.to_string()))?;
            let coord = Coordinate::parse(&args.target)
                .map_err(|e| ToolError::InvalidParams(e.to_string()))?
                .normalize_against(&session.root);

            match args.direction.as_str() {
                "up" => {
                    let (callers, value_uses) = graph.upstream(&coord);
                    Ok(format_upstream(&graph, &coord, &callers, &value_uses))
                }
                "down" => {
                    let callees = graph.callees(&coord);
                    Ok(format_downstream(&graph, &coord, &callees))
                }
                "uses" => {
                    let mentioners = graph.mentioners(&coord);
                    Ok(format_uses(&graph, &coord, &mentioners))
                }
                other => Err(ToolError::InvalidParams(format!(
                    "Invalid direction '{}': expected 'up', 'down', or 'uses'",
                    other
                ))),
            }
        }
        "mimori_memory" => {
            let args: MemoryToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_memory': {}", e))
            })?;
            let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;
            let mut ledger = crate::memory::MemoryLedger::load(&scope_dir)
                .map_err(|e| ToolError::Execution(format!("Load memory: {}", e)))?;

            match args.action.as_str() {
                "show" => {
                    let text = if let Some(sec) = args.section.as_deref() {
                        match ledger.get_section(sec) {
                            Some(content) => content,
                            None => format!("MEM_EMPTY: section '{}' not found; exit 0.", sec),
                        }
                    } else {
                        ledger.raw_content
                    };
                    if let Some(b) = args.budget {
                        let max_chars = b * 4;
                        if text.len() > max_chars {
                            let mut truncated = String::new();
                            for line in text.lines() {
                                if truncated.len() + line.len() + 1 > max_chars {
                                    truncated.push_str("… [truncated to fit token budget]\n");
                                    break;
                                }
                                truncated.push_str(line);
                                truncated.push('\n');
                            }
                            return Ok(truncated);
                        }
                    }
                    Ok(text)
                }
                "lint" => {
                    let report = ledger.lint();
                    Ok(report.to_m2m_output())
                }
                "resolve" => {
                    let target = args.target.ok_or_else(|| {
                        ToolError::InvalidParams(
                            "Missing required 'target' parameter for action 'resolve'".to_string(),
                        )
                    })?;
                    let deleted = ledger
                        .resolve(&target)
                        .map_err(|e| ToolError::Execution(format!("Resolve: {}", e)))?;
                    Ok(format!(
                        "MEM_RESOLVE: deleted {} lines matching '{}'; debt: {}/30 lines; exit 0.",
                        deleted,
                        target,
                        ledger.raw_debt_lines.len()
                    ))
                }
                other => Err(ToolError::InvalidParams(format!(
                    "Invalid action '{}': expected 'show', 'lint', or 'resolve'",
                    other
                ))),
            }
        }
        "mimori_debt" => {
            let args: DebtToolArgs = serde_json::from_value(arguments.clone()).map_err(|e| {
                ToolError::InvalidParams(format!("Invalid arguments for 'mimori_debt': {}", e))
            })?;
            let scope_dir = resolve_workspace_scope(args.workspace_dir.as_deref(), &session.root)?;

            match args.action.as_str() {
                "list" => {
                    let (_markers, output) =
                        crate::memory::list_debt(&scope_dir, args.scope.as_deref());
                    Ok(output)
                }
                "check" => {
                    let (_passed, output) = crate::memory::check_debt(&scope_dir);
                    Ok(output)
                }
                "sync" => {
                    let (_in_code, _manual, _synced, output) = crate::memory::sync_debt(&scope_dir)
                        .map_err(|e| ToolError::Execution(format!("Debt sync: {}", e)))?;
                    Ok(output)
                }
                other => Err(ToolError::InvalidParams(format!(
                    "Invalid action '{}': expected 'list', 'check', or 'sync'",
                    other
                ))),
            }
        }
        unknown => Err(ToolError::NotFound(unknown.to_string())),
    }
}
