use crate::graph::SymbolGraph;
use crate::model::{Coordinate, Symbol, SymbolKind};
use crate::workspace::walker::MAX_INDEX_FILE_BYTES;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
    #[serde(default)]
    pub is_sink: bool,
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
    #[serde(default = "default_blast_direction")]
    pub direction: String,
    #[serde(default)]
    pub value_uses: Vec<BlastNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub literal_test_mentions: Vec<String>,
    /// Honesty warning emitted when querying a method whose leaf name has
    /// unresolved cross-file untyped `.method()` call sites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_member_calls: Option<String>,
    /// JIT scoped memory notes anchored to the target file or symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_notes: Vec<String>,
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

        if let Some(warn) = &self.unverified_member_calls {
            out.push_str(&format!("> {}\n\n", warn));
        }

        if !self.memory_notes.is_empty() {
            out.push_str("### Scoped Memory\n");
            for note in &self.memory_notes {
                out.push_str(&format!("- {}\n", note));
            }
            out.push('\n');
        }

        if self.affected.is_empty() && self.value_uses.is_empty() {
            if self.direction == "down" {
                out.push_str("No downstream callees reached. This symbol is a leaf or calls nothing tracked.\n");
            } else {
                out.push_str(
                    "No upstream callers affected. This symbol is an isolated root or leaf.\n",
                );
            }
            if !self.literal_test_mentions.is_empty() {
                out.push_str("\n### Literal / Source-Inspection Test Mentions\n");
                for m in &self.literal_test_mentions {
                    out.push_str(&format!("- `{}`\n", m));
                }
            }
            return out;
        }

        if !self.entry_points.is_empty() {
            if self.direction == "down" {
                out.push_str("### Boundary Sinks\n");
            } else {
                out.push_str("### Affected Entry Points\n");
            }
            for ep in &self.entry_points {
                out.push_str(&format!("- `{}`\n", ep));
            }
            out.push('\n');
        }

        if !self.test_suites.is_empty() {
            out.push_str("### Affected Test Suites\n");
            for ts in &self.test_suites {
                out.push_str(&format!("- `{}`\n", ts));
            }
            out.push('\n');
        }

        if !self.affected.is_empty() {
            out.push_str("### Transitive Call Tree\n");
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
                    "{}- (d={}) **`{}`** ({}) -> `{}`{}{}\n",
                    indent, node.depth, node.name, node.kind, node.coordinate, call_at, tag
                ));
            }
        }
        out.push('\n');

        if !self.value_uses.is_empty() {
            out.push_str("### Value Uses (mentions — weakest tier, not traversed)\n");
            for node in &self.value_uses {
                out.push_str(&format!(
                    "- **`{}`** ({}) -> `{}` [Value Use]\n",
                    node.name, node.kind, node.coordinate
                ));
            }
            out.push('\n');
        }

        if !self.literal_test_mentions.is_empty() {
            out.push_str("### Literal / Source-Inspection Test Mentions\n");
            for m in &self.literal_test_mentions {
                out.push_str(&format!("- `{}`\n", m));
            }
            out.push('\n');
        }

        out
    }

    /// Files touched by this blast result (for scoping `--with-sinks` sweeps).
    pub fn cone_files(&self, target_file: Option<&str>) -> HashSet<String> {
        let mut files = HashSet::new();
        if let Some(f) = target_file {
            files.insert(f.to_string());
        }
        for n in &self.affected {
            files.insert(n.file.clone());
        }
        for n in &self.value_uses {
            files.insert(n.file.clone());
        }
        files
    }
}

pub fn calculate_blast_radius(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
) -> Result<BlastResult> {
    traverse(graph, coord, depth_limit, Traversal::Upstream)
}

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
    "is_empty", "clone", "fmt", "eq", "cmp", "hash", "deref", "index", "add", "sub", "main",
    "test", "tests", "error", "result", "value", "data", "item", "name", "path", "file", "line",
    "args", "root", "node", "state", "count", "check", "start", "stop",
];

fn is_generic_leaf_ident(leaf: &str) -> bool {
    leaf.len() < 4 || GENERIC_LEAF_NAMES.contains(&leaf)
}

const MAX_FILE_CACHE_ENTRIES: usize = 64;
const MAX_FILE_CACHE_BYTES: usize = 16 * 1024 * 1024;

struct FileLineCache<'a> {
    root: Option<&'a Path>,
    files: HashMap<&'a str, (Vec<String>, usize)>,
    order: VecDeque<&'a str>,
    cached_bytes: usize,
}

impl<'a> FileLineCache<'a> {
    fn new(root: Option<&'a Path>) -> Self {
        Self {
            root,
            files: HashMap::new(),
            order: VecDeque::new(),
            cached_bytes: 0,
        }
    }

    fn load_lines(&self, rel_file: &str) -> (Vec<String>, usize) {
        let Some(root) = self.root else {
            return (Vec::new(), 0);
        };
        let Ok(path) = crate::workspace::walker::confine_to_workspace(root, Path::new(rel_file))
        else {
            return (Vec::new(), 0);
        };
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_INDEX_FILE_BYTES) {
            return (Vec::new(), 0);
        }
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let bytes = s.len();
                let lines = s.lines().map(str::to_string).collect();
                (lines, bytes)
            }
            Err(_) => (Vec::new(), 0),
        }
    }

    fn evict_if_needed(&mut self, incoming_bytes: usize) {
        while (self.files.len() >= MAX_FILE_CACHE_ENTRIES
            || self.cached_bytes.saturating_add(incoming_bytes) > MAX_FILE_CACHE_BYTES)
            && !self.order.is_empty()
        {
            if let Some(oldest) = self.order.pop_front() {
                if let Some((_, bytes)) = self.files.remove(oldest) {
                    self.cached_bytes = self.cached_bytes.saturating_sub(bytes);
                }
            }
        }
    }

    fn emit_symbol_range<F>(sym: &Symbol, lines: &[String], mut f: F)
    where
        F: FnMut(usize, &str),
    {
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
        if self.root.is_none() {
            return;
        }
        if let Some((lines, _)) = self.files.get(sym.file.as_str()) {
            Self::emit_symbol_range(sym, lines, f);
            return;
        }
        let (lines, bytes) = self.load_lines(&sym.file);
        if bytes > MAX_FILE_CACHE_BYTES {
            Self::emit_symbol_range(sym, &lines, f);
            return;
        }
        self.evict_if_needed(bytes);
        self.cached_bytes = self.cached_bytes.saturating_add(bytes);
        self.order.push_back(sym.file.as_str());
        let entry = self
            .files
            .entry(sym.file.as_str())
            .or_insert((lines, bytes));
        Self::emit_symbol_range(sym, &entry.0, f);
    }
}

struct BfsOutput {
    affected: Vec<BlastNode>,
    entry_points: Vec<String>,
    test_suites: Vec<String>,
    visited: HashSet<usize>,
}

fn run_bfs<'a>(
    graph: &'a SymbolGraph,
    seed_indices: &[usize],
    exclude_set: &HashSet<usize>,
    depth_limit: usize,
    direction: Traversal,
    file_cache: &mut FileLineCache<'a>,
) -> BfsOutput {
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

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    for &idx in seed_indices {
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
        let neighbors = match direction {
            Traversal::Upstream => graph.callers_map.get(&curr_idx),
            Traversal::Downstream => graph.callees_map.get(&curr_idx),
        };
        let Some(next) = neighbors else {
            continue;
        };

        for &next_idx in next {
            let sym = &graph.symbols[next_idx];
            let (is_ep, is_sk) = match direction {
                Traversal::Upstream => (is_entry_point(graph, next_idx, sym), false),
                Traversal::Downstream => (false, is_sink(graph, next_idx)),
            };
            let is_test = is_test_symbol(sym);

            if exclude_set.contains(&next_idx) {
                if next_idx != curr_idx {
                    if (is_ep || is_sk) && seen_entry_indices.insert(next_idx) {
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
            if (is_ep || is_sk) && seen_entry_indices.insert(next_idx) {
                entry_points.push(sym.coordinate());
            }
            if is_test && seen_test_suites.insert(sym.file.clone()) {
                test_suites.push(sym.file.clone());
            }

            let call_lines = if direction == Traversal::Upstream && next_depth == 1 {
                find_call_site_lines(file_cache, sym, &call_target_names)
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

    affected.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.coordinate.cmp(&b.coordinate))
    });
    entry_points.sort();
    test_suites.sort();

    BfsOutput {
        affected,
        entry_points,
        test_suites,
        visited,
    }
}

pub(crate) fn build_unverified_member_warning(
    graph: &SymbolGraph,
    seed_indices: &[usize],
) -> Option<String> {
    let mut warned: BTreeMap<&str, usize> = BTreeMap::new();
    for &idx in seed_indices {
        let sym = &graph.symbols[idx];
        if sym.kind != SymbolKind::Method {
            continue;
        }
        let leaf = sym.name.rsplit([':', '.']).next().unwrap_or(&sym.name);
        if let Some(&count) = graph.unresolved_member_calls.get(leaf) {
            if count > 0 {
                warned.entry(leaf).or_insert(count);
            }
        }
    }
    if warned.is_empty() {
        return None;
    }
    if warned.len() == 1 {
        let (&leaf, &count) = warned.iter().next()?;
        return Some(format!(
            "Warning: {} unresolved cross-file `.{}()` call site(s) exist in workspace (untyped receiver calls are not linked statically); verify with: `rg -n \"\\.{}\\(\"`",
            count, leaf, leaf
        ));
    }
    let total: usize = warned.values().sum();
    let details: Vec<String> = warned
        .iter()
        .map(|(leaf, count)| format!("`.{}()` ({})", leaf, count))
        .collect();
    let rg_pattern = warned.keys().copied().collect::<Vec<_>>().join("|");
    Some(format!(
        "Warning: {} unresolved cross-file member call site(s) across {} method(s) ({}) exist in workspace (untyped receiver calls are not linked statically); verify with: `rg -n \"\\.({})\\(\"`",
        total,
        warned.len(),
        details.join(", "),
        rg_pattern
    ))
}

fn traverse(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
    direction: Traversal,
) -> Result<BlastResult> {
    let indices = match direction {
        Traversal::Upstream => graph.resolve_strict_upstream(coord, "blast")?,
        Traversal::Downstream => vec![graph.resolve_strict(coord, "blast --down")?],
    };

    let mut file_cache = FileLineCache::new(graph.root.as_deref());
    let empty_exclude = HashSet::new();
    let mut bfs = run_bfs(
        graph,
        &indices,
        &empty_exclude,
        depth_limit,
        direction,
        &mut file_cache,
    );

    const MAX_VALUE_USES: usize = 100;
    let mut value_uses = Vec::new();
    let mut literal_test_mentions = Vec::new();
    let mut unverified_member_calls = None;

    if direction == Traversal::Upstream {
        unverified_member_calls = build_unverified_member_warning(graph, &indices);

        'seeds: for &seed in &indices {
            for m_idx in graph.mentioner_indices(seed) {
                if !bfs.visited.insert(m_idx) {
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

        value_uses.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));

        let literal_seed_names: Vec<&str> = indices
            .iter()
            .filter_map(|&i| {
                let full = graph.symbols[i].name.as_str();
                let leaf = full.rsplit([':', '.']).next().unwrap_or(full);
                (!is_generic_leaf_ident(leaf)).then_some(leaf)
            })
            .collect();
        literal_test_mentions = collect_literal_test_mentions(
            graph,
            &mut file_cache,
            &bfs.visited,
            &literal_seed_names,
        );
    }

    let memory_notes = graph
        .root
        .as_deref()
        .map(|r| {
            let files: Vec<&str> = indices
                .iter()
                .map(|&i| graph.symbols[i].file.as_str())
                .collect();
            let syms: Vec<&str> = indices
                .iter()
                .map(|&i| graph.symbols[i].name.as_str())
                .collect();
            crate::memory::MemoryLedger::scoped_notes_for_workspace(r, &files, &syms)
        })
        .unwrap_or_default();

    Ok(BlastResult {
        target: coord.to_string(),
        depth_limit,
        affected: bfs.affected,
        entry_points: bfs.entry_points,
        test_suites: bfs.test_suites,
        direction: match direction {
            Traversal::Upstream => "up".to_string(),
            Traversal::Downstream => "down".to_string(),
        },
        value_uses,
        literal_test_mentions,
        unverified_member_calls,
        memory_notes,
    })
}

pub(crate) struct MultiUpstreamResult {
    pub affected: Vec<BlastNode>,
    pub entry_points: Vec<String>,
    pub test_suites: Vec<String>,
    pub literal_test_mentions: Vec<String>,
    pub unverified_member_calls: Option<String>,
}

/// Multi-source upstream BFS for `mimori diff`.
pub(crate) fn traverse_multi_upstream(
    graph: &SymbolGraph,
    mutated_indices: &[usize],
    depth_limit: usize,
) -> MultiUpstreamResult {
    if mutated_indices.is_empty() {
        return MultiUpstreamResult {
            affected: Vec::new(),
            entry_points: Vec::new(),
            test_suites: Vec::new(),
            literal_test_mentions: Vec::new(),
            unverified_member_calls: None,
        };
    }

    let mut file_cache = FileLineCache::new(graph.root.as_deref());
    let mutated_set: HashSet<usize> = mutated_indices.iter().copied().collect();
    let seed_indices = graph.expand_upstream_indices(mutated_indices);

    let mut bfs = run_bfs(
        graph,
        &seed_indices,
        &mutated_set,
        depth_limit,
        Traversal::Upstream,
        &mut file_cache,
    );

    let unverified_member_calls = build_unverified_member_warning(graph, &seed_indices);

    for &seed in &seed_indices {
        for m_idx in graph.mentioner_indices(seed) {
            bfs.visited.insert(m_idx);
        }
    }

    let mut literal_seed_names: Vec<&str> = seed_indices
        .iter()
        .filter_map(|&i| {
            let full = graph.symbols[i].name.as_str();
            let leaf = full.rsplit([':', '.']).next().unwrap_or(full);
            (!is_generic_leaf_ident(leaf)).then_some(leaf)
        })
        .collect();
    literal_seed_names.sort_unstable();
    literal_seed_names.dedup();

    let literal_test_mentions =
        collect_literal_test_mentions(graph, &mut file_cache, &bfs.visited, &literal_seed_names);

    MultiUpstreamResult {
        affected: bfs.affected,
        entry_points: bfs.entry_points,
        test_suites: bfs.test_suites,
        literal_test_mentions,
        unverified_member_calls,
    }
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

    let leaf = sym.name.rsplit([':', '.']).next().unwrap_or(&sym.name);
    let is_exported = sym.is_public()
        || sym.signature.starts_with("pub ")
        || sym.signature.starts_with("pub(")
        || sym.signature.starts_with("export ")
        || (sym.file.ends_with(".go") && leaf.chars().next().is_some_and(|c| c.is_uppercase()))
        || (sym.file.ends_with(".py") && !leaf.starts_with('_'));

    has_no_callers && is_exported
}

pub(crate) fn is_program_entry(sym: &Symbol) -> bool {
    sym.name == "main" || sym.name.ends_with("::main")
}

pub(crate) fn is_sink(graph: &SymbolGraph, idx: usize) -> bool {
    graph
        .callees_map
        .get(&idx)
        .is_none_or(|callees| callees.is_empty())
}

/// AST-first test detection (`symbol_flags::IS_TEST`), falling back to path
/// components, filename suffixes, and test-function naming conventions.
pub(crate) fn is_test_symbol(sym: &Symbol) -> bool {
    if sym.is_test_flag() {
        return true;
    }
    const TEST_DIRS: &[&str] = &["tests", "test", "__tests__", "spec", "__mocks__"];
    const TEST_SUFFIXES: &[&str] = &[
        "_test.go",
        "_test.py",
        "_test.rs",
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

    if TEST_SUFFIXES.iter().any(|sfx| name.ends_with(sfx)) || name.starts_with("test_") {
        return true;
    }

    let sym_leaf = sym.name.rsplit("::").next().unwrap_or(&sym.name);
    sym_leaf.starts_with("test_")
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

/// Case-sensitive substring sweep over blast-cone files (or indexed files when
/// `cone_files` is `None`/empty). Returns `(hits, truncated)` with clean JSON data.
pub fn sweep_literal_sinks(
    root: &Path,
    graph: &SymbolGraph,
    sinks: &[String],
    cone_files: Option<&HashSet<String>>,
) -> (Vec<String>, bool) {
    if sinks.is_empty() {
        return (Vec::new(), false);
    }
    const CAP: usize = 100;
    let mut files: Vec<&str> = match cone_files {
        Some(cone) if !cone.is_empty() => cone.iter().map(String::as_str).collect(),
        _ if !graph.indexed_files.is_empty() => {
            graph.indexed_files.iter().map(String::as_str).collect()
        }
        _ => graph.symbols.iter().map(|s| s.file.as_str()).collect(),
    };
    files.sort_unstable();
    files.dedup();

    let mut hits = Vec::new();
    let mut truncated = false;
    'files: for rel in files {
        let Ok(confined) = crate::workspace::walker::confine_to_workspace(root, Path::new(rel))
        else {
            continue;
        };
        if std::fs::metadata(&confined).is_ok_and(|m| m.len() > MAX_INDEX_FILE_BYTES) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(confined) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if sinks.iter().any(|s| line.contains(s)) {
                if hits.len() >= CAP {
                    truncated = true;
                    break 'files;
                }
                hits.push(format!("{}:#L{}: {}", rel, idx + 1, line.trim()));
            }
        }
    }
    (hits, truncated)
}

pub fn format_sink_hits(sinks: &[String], hits: &[String], truncated: bool) -> String {
    let mut out = format!(
        "\n### Literal sink hits (`--with-sinks {}`)\n\n",
        sinks.join(",")
    );
    if hits.is_empty() {
        out.push_str("No literal sink hits.\n");
    } else {
        for h in hits {
            out.push_str(&format!("- `{}`\n", h));
        }
        if truncated {
            out.push_str("\n_Capped at 100 hits; refine --with-sinks or use rg._\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{symbol_flags, SymbolKind};

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
            flags: 0,
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
        let g = SymbolGraph::new(vec![
            sr("src/a.rs", "top", &["mid"]),
            sr("src/a.rs", "mid", &["leaf"]),
            s("src/a.rs", "leaf"),
        ]);
        let coord = Coordinate::parse("src/a.rs:top").unwrap();
        let res = calculate_downstream_blast(&g, &coord, 3).unwrap();
        assert_eq!(res.direction, "down");
        let names: Vec<&str> = res.affected.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"mid"));
        assert!(names.contains(&"leaf"));

        let coord = Coordinate::parse("src/a.rs:leaf").unwrap();
        let up = calculate_blast_radius(&g, &coord, 3).unwrap();
        assert_eq!(up.direction, "up");
        let names: Vec<&str> = up.affected.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"mid"));
    }

    #[test]
    fn blast_emits_honesty_warning_for_unresolved_cross_file_methods() {
        let mut m = s("src/db.rs", "Database::save_rows");
        m.kind = SymbolKind::Method;
        let mut caller = sr("src/app.rs", "run_app", &["save_rows"]);
        caller.member_calls = vec!["save_rows".to_string()];
        let g = SymbolGraph::new(vec![m, caller]);
        let res = calculate_blast_radius(
            &g,
            &Coordinate::parse("src/db.rs:Database::save_rows").unwrap(),
            3,
        )
        .unwrap();
        assert!(
            res.unverified_member_calls
                .as_deref()
                .is_some_and(|w| w.contains("1 unresolved cross-file `.save_rows()`")),
            "expected honesty warning, got: {:?}",
            res.unverified_member_calls
        );
    }

    #[test]
    fn test_detection_respects_ast_flag_and_conventions() {
        let mut flagged = s("src/lib.rs", "helper_inside_cfg_test");
        flagged.flags = symbol_flags::IS_TEST;
        assert!(is_test_symbol(&flagged));
        assert!(is_test_symbol(&s("tests/cli_map.rs", "anything")));
        assert!(is_test_symbol(&s("src/__tests__/a.ts", "anything")));
        assert!(is_test_symbol(&s("pkg/server_test.go", "TestServe")));
        assert!(is_test_symbol(&s("src/a.spec.ts", "anything")));
        assert!(is_test_symbol(&s("src/lib.rs", "test_parses_input")));

        assert!(!is_test_symbol(&s("src/latest.rs", "latest_version")));
        assert!(!is_test_symbol(&s("src/contest.rs", "run_contest")));
        assert!(!is_test_symbol(&s("src/auth.rs", "attestation")));
        assert!(!is_test_symbol(&s("src/protest/mod.rs", "handler")));
        assert!(!is_test_symbol(&s("src/Testimonials.rs", "Testimonials")));
        assert!(!is_test_symbol(&s("src/runner.rs", "TestRunner")));
    }

    #[test]
    fn blast_markdown_preserves_literal_test_mentions_when_affected_empty() {
        let res = BlastResult {
            target: "src/a.rs:lonely_token".into(),
            depth_limit: 3,
            affected: vec![],
            entry_points: vec![],
            test_suites: vec![],
            direction: "up".into(),
            value_uses: vec![],
            literal_test_mentions: vec!["tests/spec.rs:#L12".into()],
            unverified_member_calls: None,
            memory_notes: vec![],
        };
        let md = res.to_markdown();
        assert!(md.contains("Literal / Source-Inspection Test Mentions"));
        assert!(md.contains("tests/spec.rs:#L12"));
    }

    #[test]
    fn unverified_member_warning_aggregates_multiple_methods_and_generic_names() {
        let mut m1 = s("src/symbol.rs", "SymbolKind::as_str");
        m1.kind = SymbolKind::Method;
        let mut m2 = s("src/db.rs", "Database::save_rows");
        m2.kind = SymbolKind::Method;
        let mut c = sr("src/app.rs", "run_app", &["as_str", "save_rows"]);
        c.member_calls = vec!["as_str".into(), "save_rows".into()];
        let g = SymbolGraph::new(vec![m1, m2, c]);

        let single = calculate_blast_radius(
            &g,
            &Coordinate::parse("src/symbol.rs:SymbolKind::as_str").unwrap(),
            3,
        )
        .unwrap();
        assert!(single
            .unverified_member_calls
            .as_deref()
            .is_some_and(|w| w.contains(".as_str()")));

        let m1_idx = g
            .symbols
            .iter()
            .position(|x| x.name == "SymbolKind::as_str")
            .unwrap();
        let m2_idx = g
            .symbols
            .iter()
            .position(|x| x.name == "Database::save_rows")
            .unwrap();
        let multi = build_unverified_member_warning(&g, &[m1_idx, m2_idx]).unwrap();
        assert!(multi.contains(".as_str()"), "got: {multi}");
        assert!(multi.contains(".save_rows()"), "got: {multi}");
    }
}
