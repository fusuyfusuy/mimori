use crate::graph::SymbolGraph;
use crate::model::Coordinate;
use crate::model::Symbol;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastNode {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub depth: usize,
    pub is_entry_point: bool,
    pub is_test: bool,
    /// Set for downstream traversals when the node calls nothing tracked.
    /// Upstream traversals leave it false and use `is_entry_point` instead.
    #[serde(default)]
    pub is_sink: bool,
    /// Weakest tier: reached via a non-call mention (property read, type
    /// ref, arg/template identifier), not a call edge. Never traversed
    /// further, never an entry point.
    #[serde(default)]
    pub is_value_use: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub call_lines: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastResult {
    pub target: String,
    pub depth_limit: usize,
    pub affected: Vec<BlastNode>,
    pub entry_points: Vec<String>,
    pub test_suites: Vec<String>,
    /// `up` (callers) or `down` (callees). Defaults to `up` so cached JSON
    /// without the key still loads.
    #[serde(default = "default_blast_direction")]
    pub direction: String,
    /// Direct mentioners of the target (upstream only): value uses that no
    /// call edge reaches. Call edge wins on overlap — anything already in
    /// `affected` is excluded here.
    #[serde(default)]
    pub value_uses: Vec<BlastNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub literal_test_mentions: Vec<String>,
}

fn default_blast_direction() -> String {
    "up".to_string()
}

impl BlastResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let dir_label = if self.direction == "down" {
            "Downstream"
        } else {
            "Upstream"
        };
        out.push_str(&format!(
            "# Blast Radius ({dir_label}): `{}` (Depth: {}, Affected: {})\n\n",
            self.target,
            self.depth_limit,
            self.affected.len()
        ));
        out.push_str(&format!("{}\n\n", super::COUNT_LEGEND));
        if self.affected.is_empty() && self.value_uses.is_empty() {
            if self.direction == "down" {
                out.push_str("No downstream callees reached. This symbol is a leaf or calls nothing tracked.\n");
            } else {
                out.push_str(
                    "No upstream callers affected. This symbol is an isolated root or leaf.\n",
                );
            }
            return out;
        }

        if !self.entry_points.is_empty() {
            if self.direction == "down" {
                out.push_str("### 🛑 Boundary Sinks\n");
            } else {
                out.push_str("### 🚪 Affected Entry Points\n");
            }
            for ep in &self.entry_points {
                out.push_str(&format!("- 🎯 `{}`\n", ep));
            }
            out.push('\n');
        }

        if !self.test_suites.is_empty() {
            out.push_str("### 🧪 Affected Test Suites\n");
            for ts in &self.test_suites {
                out.push_str(&format!("- 🧪 `{}`\n", ts));
            }
            out.push('\n');
        }

        if !self.affected.is_empty() {
            out.push_str("### 🌊 Transitive Call Tree\n");
            for node in &self.affected {
                let indent = "  ".repeat(node.depth);
                let tag = if node.is_entry_point {
                    " [Entry Point]"
                } else if node.is_sink {
                    " [Sink]"
                } else if node.is_test {
                    " [Test]"
                } else {
                    ""
                };
                let call_at = if node.call_lines.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> =
                        node.call_lines.iter().map(|l| format!("L{l}")).collect();
                    format!(" [calls @ {}]", formatted.join(", "))
                };
                out.push_str(&format!(
                    "{}- (d={}) **`{}`** ({}) → `{}`{}{}\n",
                    indent, node.depth, node.name, node.kind, node.coordinate, call_at, tag
                ));
            }
        }
        out.push('\n');

        if !self.value_uses.is_empty() {
            out.push_str("### 📎 Value Uses (mentions — weakest tier, not traversed)\n");
            for node in &self.value_uses {
                out.push_str(&format!(
                    "- **`{}`** ({}) → `{}` [Value Use]\n",
                    node.name, node.kind, node.coordinate
                ));
            }
            out.push('\n');
        }

        if !self.literal_test_mentions.is_empty() {
            out.push_str("### 🔍 Literal / Source-Inspection Test Mentions\n");
            for m in &self.literal_test_mentions {
                out.push_str(&format!("- 🔍 `{}`\n", m));
            }
            out.push('\n');
        }

        out
    }
}

pub fn calculate_blast_radius(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
) -> Result<BlastResult> {
    traverse(graph, coord, depth_limit, Traversal::Upstream)
}

/// Transitive downstream impact: everything `target` calls, directly or
/// transitively, up to `depth_limit`. Answers "is this safe to delete / what
/// does this pull in?" — the mirror of `calculate_blast_radius`.
///
/// `entry_points` here reports reached *sinks* (symbols with no tracked
/// callees) so callers can still eyeball the boundary of the cone.
pub fn calculate_downstream_blast(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
) -> Result<BlastResult> {
    traverse(graph, coord, depth_limit, Traversal::Downstream)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Traversal {
    Upstream,
    Downstream,
}

const GENERIC_LEAF_NAMES: &[&str] = &[
    "new", "from", "default", "into", "try_from", "try_into", "with", "drop", "next", "read",
    "write", "open", "close", "load", "save", "init", "build", "parse", "run", "get", "set", "len",
    "is_empty", "clone", "fmt", "eq", "cmp", "hash", "deref", "index", "add", "sub",
];

fn is_generic_leaf_ident(leaf: &str, full_name: &str) -> bool {
    let is_qualified = full_name.contains("::") || full_name.contains('.');
    is_qualified && (leaf.len() < 4 || GENERIC_LEAF_NAMES.contains(&leaf))
}

struct FileLineCache<'a> {
    root: Option<&'a Path>,
    files: std::collections::HashMap<&'a str, Vec<String>>,
}

impl<'a> FileLineCache<'a> {
    fn new(root: Option<&'a Path>) -> Self {
        Self {
            root,
            files: std::collections::HashMap::new(),
        }
    }

    fn with_symbol_lines<F>(&mut self, sym: &'a Symbol, mut f: F)
    where
        F: FnMut(usize, &str),
    {
        if !sym.body.is_empty() {
            for (idx, line) in sym.body.lines().enumerate() {
                f(idx, line);
            }
            return;
        }
        let Some(root) = self.root else {
            return;
        };
        let lines = self.files.entry(sym.file.as_str()).or_insert_with(|| {
            let Ok(path) =
                crate::workspace::walker::confine_to_workspace(root, Path::new(&sym.file))
            else {
                return Vec::new();
            };
            std::fs::read_to_string(path)
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        if lines.is_empty() {
            return;
        }
        let start_idx = sym.start_line.saturating_sub(1);
        if start_idx >= lines.len() {
            return;
        }
        let end_idx = sym.end_line.max(sym.start_line).min(lines.len());
        for (idx, line) in lines[start_idx..end_idx].iter().enumerate() {
            f(idx, line);
        }
    }
}

fn traverse(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
    direction: Traversal,
) -> Result<BlastResult> {
    // P0: upstream blasts on a class fold in its constructor's callers, so
    // `new X(...)` sites seed the transitive cone.
    let indices = match direction {
        Traversal::Upstream => graph.resolve_upstream_targets(coord),
        Traversal::Downstream => graph.resolve_all(coord),
    };
    if indices.is_empty() {
        bail!("Target {}", graph.format_not_found_error(coord));
    }

    let mut file_cache = FileLineCache::new(graph.root.as_deref());

    let call_target_names: Vec<&str> = indices
        .iter()
        .map(|&i| {
            let name = graph.symbols[i].name.as_str();
            name.rsplit([':', '.']).next().unwrap_or(name)
        })
        .filter(|s| !s.is_empty())
        .collect();

    let literal_seed_names: Vec<&str> = indices
        .iter()
        .filter_map(|&i| {
            let full = graph.symbols[i].name.as_str();
            let leaf = full.rsplit([':', '.']).next().unwrap_or(full);
            if is_generic_leaf_ident(leaf, full) {
                None
            } else {
                Some(leaf)
            }
        })
        .collect();

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    for &idx in &indices {
        visited.insert(idx);
        queue.push_back((idx, 0));
    }

    let mut affected = Vec::new();
    let mut entry_points = Vec::new();
    let mut seen_entry_indices = HashSet::new();
    let mut test_suites = Vec::new();
    let mut seen_test_suites = HashSet::new();

    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        if curr_depth >= depth_limit {
            continue;
        }

        let neighbors = match direction {
            Traversal::Upstream => graph.callers_map.get(&curr_idx),
            Traversal::Downstream => graph.callees_map.get(&curr_idx),
        };

        if let Some(next) = neighbors {
            for &next_idx in next {
                if !visited.contains(&next_idx) {
                    visited.insert(next_idx);
                    let sym = &graph.symbols[next_idx];
                    let next_depth = curr_depth + 1;

                    let (is_ep, is_sk) = match direction {
                        Traversal::Upstream => (is_entry_point(graph, next_idx, sym), false),
                        Traversal::Downstream => (false, is_sink(graph, next_idx)),
                    };
                    let is_test = is_test_symbol(sym);

                    if (is_ep || is_sk) && seen_entry_indices.insert(next_idx) {
                        entry_points.push(sym.coordinate());
                    }
                    if is_test && seen_test_suites.insert(sym.file.clone()) {
                        test_suites.push(sym.file.clone());
                    }

                    let call_lines = if direction == Traversal::Upstream && next_depth == 1 {
                        find_call_site_lines(&mut file_cache, sym, &call_target_names)
                    } else {
                        Vec::new()
                    };

                    affected.push(BlastNode {
                        name: sym.name.clone(),
                        kind: sym.kind.as_str().to_string(),
                        file: sym.file.clone(),
                        coordinate: sym.coordinate(),
                        depth: next_depth,
                        is_entry_point: is_ep,
                        is_test,
                        is_sink: is_sk,
                        is_value_use: false,
                        call_lines,
                    });

                    queue.push_back((next_idx, next_depth));
                }
            }
        }
    }

    // Weakest tier (upstream only): direct mentioners of the seed targets.
    // Never traversed, never entry points; anything the call closure
    // already reached stays a call row (call wins).
    const MAX_VALUE_USES: usize = 100;
    let mut value_uses = Vec::new();
    let mut literal_test_mentions = Vec::new();
    if direction == Traversal::Upstream {
        'seeds: for &seed in &indices {
            for m_idx in graph.mentioner_indices(seed) {
                if !visited.insert(m_idx) {
                    continue;
                }
                let sym = &graph.symbols[m_idx];
                value_uses.push(BlastNode {
                    name: sym.name.clone(),
                    kind: sym.kind.as_str().to_string(),
                    file: sym.file.clone(),
                    coordinate: sym.coordinate(),
                    depth: 1,
                    is_entry_point: false,
                    is_test: is_test_symbol(sym),
                    is_sink: false,
                    is_value_use: true,
                    call_lines: Vec::new(),
                });
                if value_uses.len() >= MAX_VALUE_USES {
                    break 'seeds;
                }
            }
        }
        literal_test_mentions =
            collect_literal_test_mentions(graph, &mut file_cache, &visited, &literal_seed_names);
    }

    Ok(BlastResult {
        target: coord.to_string(),
        depth_limit,
        affected,
        entry_points,
        test_suites,
        direction: match direction {
            Traversal::Upstream => "up".to_string(),
            Traversal::Downstream => "down".to_string(),
        },
        value_uses,
        literal_test_mentions,
    })
}

/// Multi-source upstream BFS for `mimori diff`: computes the combined upstream
/// blast cone for `mutated_indices` in a single `O(V + E)` pass instead of
/// running `|M|` full graph traversals.
pub(crate) fn traverse_multi_upstream(
    graph: &SymbolGraph,
    mutated_indices: &[usize],
    depth_limit: usize,
) -> (Vec<BlastNode>, Vec<String>, Vec<String>, Vec<String>) {
    if mutated_indices.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    }

    let mut file_cache = FileLineCache::new(graph.root.as_deref());
    let mutated_set: HashSet<usize> = mutated_indices.iter().copied().collect();

    // Expand class/struct seeds to include constructor callers (`new X(...)`),
    // matching `resolve_upstream_targets`.
    let mut seed_indices: Vec<usize> = Vec::new();
    let mut seen_seeds: HashSet<usize> = HashSet::new();
    for &idx in mutated_indices {
        if seen_seeds.insert(idx) {
            seed_indices.push(idx);
        }
        let sym = &graph.symbols[idx];
        if matches!(
            sym.kind,
            crate::model::SymbolKind::Class | crate::model::SymbolKind::Struct
        ) {
            for sep in [".", "::"] {
                let ctor = format!("{}{sep}constructor", sym.name);
                if let Some(c_idxs) = graph.name_to_indices.get(&ctor) {
                    for &c_idx in c_idxs {
                        if graph.symbols[c_idx].file == sym.file && seen_seeds.insert(c_idx) {
                            seed_indices.push(c_idx);
                        }
                    }
                }
            }
        }
    }

    let mut call_target_names: Vec<&str> = seed_indices
        .iter()
        .map(|&i| {
            let name = graph.symbols[i].name.as_str();
            name.rsplit([':', '.']).next().unwrap_or(name)
        })
        .filter(|s| !s.is_empty())
        .collect();
    call_target_names.sort_unstable();
    call_target_names.dedup();

    let mut literal_seed_names: Vec<&str> = seed_indices
        .iter()
        .filter_map(|&i| {
            let full = graph.symbols[i].name.as_str();
            let leaf = full.rsplit([':', '.']).next().unwrap_or(full);
            if is_generic_leaf_ident(leaf, full) {
                None
            } else {
                Some(leaf)
            }
        })
        .collect();
    literal_seed_names.sort_unstable();
    literal_seed_names.dedup();

    let mut queue = VecDeque::new();
    let mut visited: HashSet<usize> = HashSet::new();
    for &idx in &seed_indices {
        visited.insert(idx);
        queue.push_back((idx, 0usize));
    }

    let mut affected = Vec::new();
    let mut entry_points = Vec::new();
    let mut seen_entry_indices = HashSet::new();
    let mut test_suites = Vec::new();
    let mut seen_test_suites = HashSet::new();

    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        if curr_depth >= depth_limit {
            continue;
        }
        let Some(callers) = graph.callers_map.get(&curr_idx) else {
            continue;
        };
        for &next_idx in callers {
            let sym = &graph.symbols[next_idx];
            let is_ep = is_entry_point(graph, next_idx, sym);
            let is_test = is_test_symbol(sym);

            // If one mutated symbol calls another mutated symbol, record any
            // entry-point / test-suite status just like per-symbol blast did,
            // while keeping mutated symbols out of `affected`.
            if mutated_set.contains(&next_idx) {
                if next_idx != curr_idx {
                    if is_ep && seen_entry_indices.insert(next_idx) {
                        entry_points.push(sym.coordinate());
                    }
                    if is_test && seen_test_suites.insert(sym.file.clone()) {
                        test_suites.push(sym.file.clone());
                    }
                }
                continue;
            }

            if !visited.insert(next_idx) {
                continue;
            }
            let next_depth = curr_depth + 1;
            if is_ep && seen_entry_indices.insert(next_idx) {
                entry_points.push(sym.coordinate());
            }
            if is_test && seen_test_suites.insert(sym.file.clone()) {
                test_suites.push(sym.file.clone());
            }
            let call_lines = if next_depth == 1 {
                find_call_site_lines(&mut file_cache, sym, &call_target_names)
            } else {
                Vec::new()
            };
            affected.push(BlastNode {
                name: sym.name.clone(),
                kind: sym.kind.as_str().to_string(),
                file: sym.file.clone(),
                coordinate: sym.coordinate(),
                depth: next_depth,
                is_entry_point: is_ep,
                is_test,
                is_sink: false,
                is_value_use: false,
                call_lines,
            });
            queue.push_back((next_idx, next_depth));
        }
    }

    affected.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.coordinate.cmp(&b.coordinate))
    });

    // Mark mentioners as visited before scanning literal test mentions,
    // matching `traverse`.
    for &seed in &seed_indices {
        for m_idx in graph.mentioner_indices(seed) {
            visited.insert(m_idx);
        }
    }
    let literal_test_mentions =
        collect_literal_test_mentions(graph, &mut file_cache, &visited, &literal_seed_names);

    (affected, entry_points, test_suites, literal_test_mentions)
}

fn find_call_site_lines<'a>(
    file_cache: &mut FileLineCache<'a>,
    caller: &'a Symbol,
    target_names: &[&str],
) -> Vec<usize> {
    if target_names.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let multi_line = caller.end_line > caller.start_line
        || (!caller.body.is_empty() && caller.body.contains('\n'));
    file_cache.with_symbol_lines(caller, |idx, line| {
        if multi_line && idx == 0 {
            return;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('*')
            || trimmed.starts_with("/*")
        {
            return;
        }
        if target_names.iter().any(|&t| has_call_token(trimmed, t)) {
            out.push(caller.start_line + idx);
        }
    });
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn has_call_token(line: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = line[start..].find(ident) {
        let abs = start + pos;
        let end = abs + ident.len();
        let prev_ok = line[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let rest = line[end..].trim_start();
        let next_ok = rest.starts_with('(')
            || rest.starts_with('!')
            || rest.starts_with("::<")
            || rest.starts_with('<');
        if prev_ok && next_ok {
            return true;
        }
        start = abs + ident.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn has_ident_token(line: &str, ident: &str) -> bool {
    if ident.len() < 3 {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = line[start..].find(ident) {
        let abs = start + pos;
        let end = abs + ident.len();
        let prev_ok = line[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let next_ok = line[end..].chars().next().is_none_or(|c| !is_ident_char(c));
        if prev_ok && next_ok {
            return true;
        }
        start = abs + ident.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn collect_literal_test_mentions<'a>(
    graph: &'a SymbolGraph,
    file_cache: &mut FileLineCache<'a>,
    visited: &HashSet<usize>,
    seed_names: &[&str],
) -> Vec<String> {
    if seed_names.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (idx, sym) in graph.symbols.iter().enumerate() {
        if visited.contains(&idx) || !is_test_symbol(sym) {
            continue;
        }
        file_cache.with_symbol_lines(sym, |line_idx, line| {
            if out.len() >= 20 {
                return;
            }
            if seed_names.iter().any(|&t| has_ident_token(line, t)) {
                let hit = format!("{}:#L{}", sym.file, sym.start_line + line_idx);
                if seen.insert(hit.clone()) {
                    out.push(hit);
                }
            }
        });
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// An entry point is a symbol nothing else calls -- which is what the graph
/// already knows -- or a program entry by name.
///
/// The previous rule matched any name starting with `get_`, `post_` or
/// `handle_`, and any file whose path contained `main.`, `index.` or `app.`,
/// so every getter in the codebase was reported as an affected entry point.
pub(crate) fn is_entry_point(graph: &SymbolGraph, idx: usize, sym: &Symbol) -> bool {
    if is_program_entry(sym) {
        return true;
    }
    if is_test_symbol(sym) {
        return false;
    }
    let has_no_callers = graph
        .callers_map
        .get(&idx)
        .is_none_or(|callers| callers.is_empty());

    let is_exported = sym.signature.starts_with("pub ")
        || sym.signature.starts_with("pub(")
        || sym.signature.starts_with("export ")
        || (sym.file.ends_with(".go") && sym.name.chars().next().is_some_and(|c| c.is_uppercase()))
        || (sym.file.ends_with(".py") && !sym.name.starts_with('_'));

    has_no_callers && is_exported
}

/// A program entry by name (`main`). Narrower than `is_entry_point`, which
/// treats every uncalled symbol as an entry — that would exclude the exact
/// isolated symbols doctor wants to flag.
pub(crate) fn is_program_entry(sym: &Symbol) -> bool {
    sym.name == "main" || sym.name.ends_with("::main")
}

/// A sink is a symbol that calls nothing tracked — the downstream mirror of
/// an entry point. Used to mark the boundary of a downstream blast cone.
pub(crate) fn is_sink(graph: &SymbolGraph, idx: usize) -> bool {
    graph
        .callees_map
        .get(&idx)
        .is_none_or(|callees| callees.is_empty())
}

/// Test detection on real conventions, matched against path components and
/// filename suffixes.
///
/// The previous rule was `name.contains("test") || file.contains("test")`,
/// which classified `latest`, `contest` and `attestation` as tests.
pub(crate) fn is_test_symbol(sym: &Symbol) -> bool {
    const TEST_DIRS: &[&str] = &["tests", "test", "__tests__", "spec", "__mocks__"];
    const TEST_SUFFIXES: &[&str] = &[
        "_test.go",
        "_test.py",
        ".test.ts",
        ".test.tsx",
        ".test.js",
        ".test.jsx",
        ".spec.ts",
        ".spec.tsx",
        ".spec.js",
        ".spec.jsx",
    ];

    let path = Path::new(&sym.file);

    if path
        .components()
        .any(|c| matches!(c, Component::Normal(n) if TEST_DIRS.iter().any(|d| OsStr::new(d) == n)))
    {
        return true;
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    if TEST_SUFFIXES.iter().any(|sfx| name.ends_with(sfx))
        || name.starts_with("test_")
        || name.starts_with("Test")
    {
        return true;
    }

    let sym_leaf = sym.name.rsplit("::").next().unwrap_or(&sym.name);
    sym_leaf.starts_with("test_") || sym_leaf.starts_with("Test")
}

pub fn parse_sink_list(raw: Option<&str>) -> Vec<String> {
    match raw {
        None => Vec::new(),
        Some(s) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
    }
}

/// Case-sensitive substring sweep over indexed files, one hit per matching
/// line, capped so a noisy sink (e.g. `console.`) can't flood the context.
pub fn sweep_literal_sinks(root: &Path, graph: &SymbolGraph, sinks: &[String]) -> Vec<String> {
    if sinks.is_empty() {
        return Vec::new();
    }
    const CAP: usize = 100;
    let files: Vec<&str> = if !graph.indexed_files.is_empty() {
        graph.indexed_files.iter().map(String::as_str).collect()
    } else {
        let mut f: Vec<&str> = graph.symbols.iter().map(|s| s.file.as_str()).collect();
        f.sort_unstable();
        f.dedup();
        f
    };
    let mut hits = Vec::new();
    'files: for rel in files {
        let Ok(confined) = crate::workspace::walker::confine_to_workspace(root, Path::new(rel))
        else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(confined) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if sinks.iter().any(|s| line.contains(s)) {
                hits.push(format!("{}:#L{}: {}", rel, idx + 1, line.trim()));
                if hits.len() >= CAP {
                    hits.push(format!(
                        "… capped at {} hits; refine --with-sinks or use rg.",
                        CAP
                    ));
                    break 'files;
                }
            }
        }
    }
    hits
}

pub fn format_sink_hits(sinks: &[String], hits: &[String]) -> String {
    let mut out = format!(
        "\n### 🔍 Literal sink hits (`--with-sinks {}`)\n\n",
        sinks.join(",")
    );
    if hits.is_empty() {
        out.push_str("No literal sink hits.\n");
    } else {
        for h in hits {
            out.push_str(&format!("- `{}`\n", h));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;

    fn s(file: &str, name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 1,
            signature: String::new(),
            body: String::new(),
            centrality: 0.0,
            calls: vec![],
            mentions: vec![],
            call_counts: Default::default(),
            member_calls: vec![],
            external_imports: crate::model::symbol::empty_external_imports(),
        }
    }

    fn sr(file: &str, name: &str, refs: &[&str]) -> Symbol {
        let mut sym = s(file, name);
        sym.calls = refs.iter().map(|r| r.to_string()).collect();
        for r in refs {
            sym.call_counts.insert(r.to_string(), 1);
        }
        sym
    }

    #[test]
    fn downstream_blast_follows_callees_transitively() {
        use crate::graph::SymbolGraph;
        use crate::model::Coordinate;
        let g = SymbolGraph::new(vec![
            sr("src/a.rs", "top", &["mid"]),
            sr("src/a.rs", "mid", &["leaf"]),
            s("src/a.rs", "leaf"),
        ]);
        let coord = Coordinate::parse("src/a.rs:top").unwrap();
        let res = calculate_downstream_blast(&g, &coord, 3).unwrap();
        assert_eq!(res.direction, "down");
        let names: Vec<&str> = res.affected.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"mid"), "expected mid in {:?}", names);
        assert!(names.contains(&"leaf"), "expected leaf in {:?}", names);
        // Upstream from the leaf mirrors back up.
        let coord = Coordinate::parse("src/a.rs:leaf").unwrap();
        let up = calculate_blast_radius(&g, &coord, 3).unwrap();
        assert_eq!(up.direction, "up");
        let names: Vec<&str> = up.affected.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"mid"), "expected mid in {:?}", names);
    }

    #[test]
    fn test_detection_uses_conventions_not_substrings() {
        assert!(is_test_symbol(&s("tests/cli_map.rs", "anything")));
        assert!(is_test_symbol(&s("src/__tests__/a.ts", "anything")));
        assert!(is_test_symbol(&s("pkg/server_test.go", "TestServe")));
        assert!(is_test_symbol(&s("src/a.spec.ts", "anything")));
        assert!(is_test_symbol(&s("src/lib.rs", "test_parses_input")));

        // Regression M13: these are not tests.
        assert!(!is_test_symbol(&s("src/latest.rs", "latest_version")));
        assert!(!is_test_symbol(&s("src/contest.rs", "run_contest")));
        assert!(!is_test_symbol(&s("src/auth.rs", "attestation")));
        assert!(!is_test_symbol(&s("src/protest/mod.rs", "handler")));
    }
}
