pub mod blast;
pub mod diff;
pub mod doctor;
pub mod map;
pub mod missing;
pub mod pagerank;

use crate::model::{Coordinate, InlinedCallee, SliceResult, Symbol, SymbolKind};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Edge-resolution accounting.
#[derive(Debug, Clone, Default)]
pub struct ResolveStats {
    pub resolved: usize,
    pub ambiguous_dropped: usize,
    pub unresolved: usize,
    /// Calls matching a name the caller's file imports from an external
    /// (non-relative) module: never resolved locally, by design.
    pub external: usize,
}

/// Only these kinds may receive call edges. Data (`Variable`, `Field`) is never callable.
pub fn is_callable_kind(kind: &SymbolKind) -> bool {
    use SymbolKind::*;
    matches!(kind, Function | Method | Class | Struct | Enum)
}

/// Outbound-edge walk retention factor for test-file callers in PageRank.
pub const TEST_EDGE_WEIGHT: f64 = 0.25;

/// Common stdlib/collection method names that must NEVER bind via receiverless
/// Tier-1 same-file member matching (e.g., `vec.is_empty()` -> `AliasSet::is_empty`).
/// Calls with explicit/inferred receiver types (`Type::is_empty`) are qualified and
/// bypass this filter cleanly.
const GENERIC_MEMBER_NAMES: &[&str] = &[
    "is_empty",
    "push",
    "pop",
    "parse",
    "len",
    "get",
    "set",
    "new",
    "default",
    "clone",
    "fmt",
    "drop",
    "write",
    "read",
    "insert",
    "remove",
    "contains",
    "iter",
    "collect",
    "into",
    "from",
    "unwrap",
    "expect",
    "to_string",
    "as_str",
    "trim",
    "split",
    "join",
    "clear",
    "extend",
    "sort",
    "dedup",
    "filter",
    "map",
    "find",
    "any",
    "all",
    "ok",
    "err",
    "starts_with",
    "ends_with",
    "replace",
    "chars",
    "bytes",
    "lines",
    "next",
    "count",
    "first",
    "last",
    "rev",
    "take",
    "skip",
    "zip",
    "enumerate",
    "flatten",
    "fold",
    "sum",
    "min",
    "max",
    "keys",
    "values",
    "entry",
    "or_default",
    "or_insert",
    "retain",
    "truncate",
    "resize",
    "capacity",
    "with_capacity",
];

const MAX_INLINED_LOCALS: usize = 16;

/// Coverage honesty for unindexed repos: what the walker crawled vs indexed.
#[derive(Debug, Clone, Default)]
pub struct CoverageStats {
    pub indexed_files: usize,
    pub crawled_files: usize,
    pub unindexed_exts: Vec<String>,
}

/// Sublinear multiplicity weight: a 50-callsite dependency outweighs a
/// 1-callsite one, but not 50x. Cap 8.
pub fn multiplicity_weight(count: u32) -> f64 {
    (count.max(1) as f64).sqrt().min(8.0)
}

/// Counting-unit legend emitted on read headers.
pub const COUNT_LEGEND: &str =
    "*Units*: N = distinct symbols (callers/affected/mentioners); edge weights sum call-site multiplicity min(sqrt(n),8); blast affected = transitive closure over call edges only.";

#[derive(Debug, Clone)]
pub struct SymbolGraph {
    pub symbols: Vec<Symbol>,
    pub callers_map: HashMap<usize, Vec<usize>>,
    pub callees_map: HashMap<usize, Vec<usize>>,
    /// Exact-name and qualified-suffix -> symbol indices.
    pub name_to_indices: HashMap<String, Vec<usize>>,
    /// Reverse index: mentioned identifier -> symbol indices whose `mentions` contain it.
    pub mention_to_indices: HashMap<String, Vec<usize>>,
    /// Unresolved untyped receiver `.method()` call-site counts keyed by leaf method name.
    /// Powers `blast` honesty warnings and prevents `doctor` false positives on cross-file methods.
    pub unresolved_member_calls: HashMap<String, usize>,
    pub resolve_stats: ResolveStats,
    pub pagerank_iters: usize,
    pub pagerank_converged: bool,
    pub edge_weights: HashMap<(usize, usize), f64>,
    pub coverage: Option<CoverageStats>,
    /// Confined workspace root when loaded from disk (`get_or_sync_graph`).
    pub root: Option<PathBuf>,
    /// Sorted workspace-relative paths of all indexed files.
    pub indexed_files: Vec<String>,
}

struct EdgeBuilder {
    callers_map: HashMap<usize, Vec<usize>>,
    callees_map: HashMap<usize, Vec<usize>>,
    edge_weights: HashMap<(usize, usize), f64>,
    seen_edges: HashSet<(usize, usize)>,
    unresolved_member_calls: HashMap<String, usize>,
    stats: ResolveStats,
}

impl EdgeBuilder {
    fn new() -> Self {
        Self {
            callers_map: HashMap::new(),
            callees_map: HashMap::new(),
            edge_weights: HashMap::new(),
            seen_edges: HashSet::new(),
            unresolved_member_calls: HashMap::new(),
            stats: ResolveStats::default(),
        }
    }

    fn add_weighted_edge(&mut self, caller_idx: usize, callee_idx: usize, weight: f64) {
        if self.seen_edges.insert((caller_idx, callee_idx)) {
            self.callers_map
                .entry(callee_idx)
                .or_default()
                .push(caller_idx);
            self.callees_map
                .entry(caller_idx)
                .or_default()
                .push(callee_idx);
        }
        self.edge_weights
            .entry((caller_idx, callee_idx))
            .and_modify(|e| *e = e.max(weight))
            .or_insert(weight);
    }

    fn record_unresolved_member(&mut self, lookup_name: &str) {
        let leaf = lookup_name.rsplit([':', '.']).next().unwrap_or(lookup_name);
        if !leaf.is_empty() && !GENERIC_MEMBER_NAMES.contains(&leaf) {
            *self
                .unresolved_member_calls
                .entry(leaf.to_string())
                .or_insert(0) += 1;
        }
    }
}

fn push_name_idx(map: &mut HashMap<String, Vec<usize>>, key: &str, idx: usize) {
    if key.is_empty() {
        return;
    }
    let list = map.entry(key.to_string()).or_default();
    if list.last() != Some(&idx) {
        list.push(idx);
    }
}

fn build_name_and_mention_indices(
    symbols: &[Symbol],
) -> (HashMap<String, Vec<usize>>, HashMap<String, Vec<usize>>) {
    let mut name_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    let mut mention_to_indices: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, sym) in symbols.iter().enumerate() {
        push_name_idx(&mut name_to_indices, &sym.name, idx);
        for (pos, _) in sym.name.match_indices("::") {
            push_name_idx(&mut name_to_indices, &sym.name[pos + 2..], idx);
        }
        for (pos, _) in sym.name.match_indices('.') {
            push_name_idx(&mut name_to_indices, &sym.name[pos + 1..], idx);
        }
        for m in &sym.mentions {
            let list = mention_to_indices.entry(m.clone()).or_default();
            if list.last() != Some(&idx) {
                list.push(idx);
            }
        }
    }
    (name_to_indices, mention_to_indices)
}

fn build_callable_index<'a>(
    symbols: &[Symbol],
    name_to_indices: &'a HashMap<String, Vec<usize>>,
    sym_file_id: &[u32],
) -> HashMap<&'a str, Vec<(u32, usize)>> {
    let mut by_name: HashMap<&'a str, Vec<(u32, usize)>> =
        HashMap::with_capacity(name_to_indices.len());
    for (name, indices) in name_to_indices {
        let mut v: Vec<(u32, usize)> = indices
            .iter()
            .copied()
            .filter(|&i| is_callable_kind(&symbols[i].kind))
            .map(|i| (sym_file_id[i], i))
            .collect();
        if !v.is_empty() {
            v.sort_unstable();
            by_name.insert(name.as_str(), v);
        }
    }
    by_name
}

fn is_external_call(ref_name: &str, is_member: bool, external_set: &HashSet<&str>) -> bool {
    if external_set.is_empty() {
        return false;
    }
    if !is_member && external_set.contains(ref_name) {
        return true;
    }
    if let Some((prefix, _)) = ref_name.split_once("::") {
        if external_set.contains(prefix) {
            return true;
        }
    }
    if let Some((prefix, _)) = ref_name.split_once('.') {
        if external_set.contains(prefix) {
            return true;
        }
    }
    false
}

fn is_cross_file_visible(caller_is_test: bool, same_dir: bool, target: &Symbol) -> bool {
    if !caller_is_test && blast::is_test_symbol(target) {
        return false;
    }
    // Synthetic test fixtures without parsed signatures remain visible.
    if target.signature.is_empty() {
        return true;
    }
    if target.is_public() {
        return true;
    }
    // Go package-private symbols are callable within the same directory.
    same_dir && target.file.ends_with(".go")
}

struct ResolveCtx<'a> {
    symbols: &'a [Symbol],
    sym_file_id: &'a [u32],
    sym_dir: &'a [String],
    callable_by_name: &'a HashMap<&'a str, Vec<(u32, usize)>>,
}

impl<'a> ResolveCtx<'a> {
    fn resolve_call(
        &self,
        u_idx: usize,
        ref_name: &str,
        is_member: bool,
        w: f64,
        eb: &mut EdgeBuilder,
    ) {
        let (mod_hint, lookup_name) = match ref_name
            .split_once(':')
            .filter(|_| !ref_name.contains("::"))
        {
            Some((m, n)) => (Some(m), n),
            None => (None, ref_name),
        };
        let Some(callable) = self.callable_by_name.get(lookup_name) else {
            if is_member {
                eb.record_unresolved_member(lookup_name);
            }
            eb.stats.unresolved += 1;
            return;
        };

        if let Some(hint) = mod_hint {
            self.resolve_tier0(u_idx, hint, callable, w, eb);
            return;
        }

        let is_unqualified = !lookup_name.contains("::") && !lookup_name.contains('.');
        if is_member && is_unqualified && GENERIC_MEMBER_NAMES.contains(&lookup_name) {
            eb.stats.ambiguous_dropped += 1;
            return;
        }

        if self.try_resolve_same_file(
            u_idx,
            ref_name,
            lookup_name,
            is_unqualified,
            is_member,
            callable,
            w,
            eb,
        ) {
            return;
        }

        if is_member {
            eb.record_unresolved_member(lookup_name);
            eb.stats.ambiguous_dropped += 1;
            return;
        }

        self.resolve_cross_file(
            u_idx,
            ref_name,
            lookup_name,
            is_unqualified,
            callable,
            w,
            eb,
        );
    }

    fn resolve_tier0(
        &self,
        u_idx: usize,
        hint: &str,
        callable: &[(u32, usize)],
        w: f64,
        eb: &mut EdgeBuilder,
    ) {
        let u_file = self.sym_file_id[u_idx];
        let foreign: Vec<usize> = callable
            .iter()
            .filter(|&&(f, _)| f != u_file)
            .map(|&(_, v)| v)
            .collect();

        let target = if hint.is_empty() {
            (foreign.len() == 1).then(|| foreign[0])
        } else {
            let hinted: Vec<usize> = foreign
                .into_iter()
                .filter(|&v| module_hint_matches(&self.symbols[v].file, hint))
                .collect();
            (hinted.len() == 1).then(|| hinted[0])
        };

        if let Some(v_idx) = target {
            eb.add_weighted_edge(u_idx, v_idx, w);
            eb.stats.resolved += 1;
        } else {
            eb.stats.ambiguous_dropped += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn try_resolve_same_file(
        &self,
        u_idx: usize,
        ref_name: &str,
        lookup_name: &str,
        is_unqualified: bool,
        is_member: bool,
        callable: &[(u32, usize)],
        w: f64,
        eb: &mut EdgeBuilder,
    ) -> bool {
        let u_file = self.sym_file_id[u_idx];
        let pos = callable.partition_point(|&(f, _)| f < u_file);
        let mut same_file: Vec<usize> = Vec::new();
        let mut i = pos;
        while i < callable.len() && callable[i].0 == u_file {
            let v = callable[i].1;
            if !is_unqualified
                || is_member
                || self.symbols[v].kind != SymbolKind::Method
                || self.symbols[v].name == lookup_name
            {
                same_file.push(v);
            }
            i += 1;
        }

        let has_self = same_file.contains(&u_idx);
        if has_self && self.symbols[u_idx].name == ref_name {
            eb.stats.resolved += 1;
            return true;
        }
        same_file.retain(|&v| v != u_idx);

        if same_file.len() == 1 {
            eb.add_weighted_edge(u_idx, same_file[0], w);
            eb.stats.resolved += 1;
            true
        } else if same_file.len() > 1 {
            eb.stats.ambiguous_dropped += 1;
            true
        } else if has_self && is_member {
            eb.stats.resolved += 1;
            true
        } else {
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_cross_file(
        &self,
        u_idx: usize,
        ref_name: &str,
        lookup_name: &str,
        is_unqualified: bool,
        callable: &[(u32, usize)],
        w: f64,
        eb: &mut EdgeBuilder,
    ) {
        let caller_is_test = blast::is_test_symbol(&self.symbols[u_idx]);
        let u_dir = &self.sym_dir[u_idx];

        let cross_base: Vec<(u32, usize)> = callable
            .iter()
            .copied()
            .filter(|&(_, v)| {
                !is_unqualified
                    || self.symbols[v].kind != SymbolKind::Method
                    || self.symbols[v].name == lookup_name
            })
            .collect();
        if cross_base.is_empty() {
            eb.stats.unresolved += 1;
            return;
        }

        let fuzzy_ok = ref_name != "new" && ref_name != "default";
        if !fuzzy_ok {
            eb.stats.ambiguous_dropped += 1;
            return;
        }

        // Tier 2: same directory.
        let mut same_dir: Vec<usize> = cross_base
            .iter()
            .filter(|&&(_, v)| {
                v != u_idx
                    && &self.sym_dir[v] == u_dir
                    && is_cross_file_visible(caller_is_test, true, &self.symbols[v])
            })
            .map(|&(_, v)| v)
            .collect();
        same_dir.sort_unstable();
        same_dir.dedup();
        if same_dir.len() == 1 {
            eb.add_weighted_edge(u_idx, same_dir[0], w);
            eb.stats.resolved += 1;
            return;
        } else if same_dir.len() > 1 {
            eb.stats.ambiguous_dropped += 1;
            return;
        }

        // Tier 3: unique global across visible cross-file symbols.
        let mut global: Vec<usize> = cross_base
            .iter()
            .filter(|&&(_, v)| {
                v != u_idx && is_cross_file_visible(caller_is_test, false, &self.symbols[v])
            })
            .map(|&(_, v)| v)
            .collect();
        global.sort_unstable();
        global.dedup();
        if global.len() == 1 {
            eb.add_weighted_edge(u_idx, global[0], w);
            eb.stats.resolved += 1;
        } else if global.is_empty() {
            eb.stats.unresolved += 1;
        } else {
            eb.stats.ambiguous_dropped += 1;
        }
    }
}

impl SymbolGraph {
    pub fn new(mut symbols: Vec<Symbol>) -> Self {
        let _p = crate::Phase::start("  name index");
        symbols.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.end_line.cmp(&b.end_line))
                .then(a.name.cmp(&b.name))
                .then(a.kind.as_str().cmp(b.kind.as_str()))
        });

        let (name_to_indices, mention_to_indices) = build_name_and_mention_indices(&symbols);
        drop(_p);

        let _p = crate::Phase::start("  edge resolve");
        let mut file_ids: HashMap<&str, u32> = HashMap::new();
        let sym_file_id: Vec<u32> = symbols
            .iter()
            .map(|s| {
                let next = file_ids.len() as u32;
                *file_ids.entry(s.file.as_str()).or_insert(next)
            })
            .collect();
        let sym_dir: Vec<String> = symbols
            .iter()
            .map(|s| {
                Path::new(&s.file)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();

        let callable_by_name = build_callable_index(&symbols, &name_to_indices, &sym_file_id);
        let ctx = ResolveCtx {
            symbols: &symbols,
            sym_file_id: &sym_file_id,
            sym_dir: &sym_dir,
            callable_by_name: &callable_by_name,
        };
        let mut eb = EdgeBuilder::new();
        let mut cached_ext_ptr: usize = 0;
        let mut external_set: HashSet<&str> = HashSet::new();

        for (u_idx, sym) in symbols.iter().enumerate() {
            if sym.calls.is_empty() {
                continue;
            }
            let ext_ptr = std::sync::Arc::as_ptr(&sym.external_imports) as *const () as usize;
            if ext_ptr != cached_ext_ptr {
                external_set.clear();
                external_set.extend(sym.external_imports.iter().map(|s| s.as_str()));
                cached_ext_ptr = ext_ptr;
            }
            let member_set: HashSet<&str> = sym.member_calls.iter().map(|s| s.as_str()).collect();

            for ref_name in &sym.calls {
                let is_member = member_set.contains(ref_name.as_str());
                if is_external_call(ref_name, is_member, &external_set) {
                    eb.stats.external += 1;
                    continue;
                }
                let count = sym.call_counts.get(ref_name).copied().unwrap_or(1);
                let w = multiplicity_weight(count);
                ctx.resolve_call(u_idx, ref_name, is_member, w, &mut eb);
            }
        }
        drop(_p);

        let _p = crate::Phase::start("  pagerank");
        let pr = pagerank::compute_weighted_pagerank(
            &mut symbols,
            &eb.callers_map,
            &eb.callees_map,
            &eb.edge_weights,
            None,
        );
        drop(_p);

        SymbolGraph {
            symbols,
            callers_map: eb.callers_map,
            callees_map: eb.callees_map,
            name_to_indices,
            mention_to_indices,
            unresolved_member_calls: eb.unresolved_member_calls,
            resolve_stats: eb.stats,
            pagerank_iters: pr.iters,
            pagerank_converged: pr.converged,
            edge_weights: eb.edge_weights,
            coverage: None,
            root: None,
            indexed_files: Vec::new(),
        }
    }

    pub fn seed_indices(&self, term: &str) -> Vec<usize> {
        let needle = term.to_lowercase();
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.name.to_lowercase().contains(&needle) || s.file.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn apply_personalization(&mut self, indices: &[usize]) {
        let pr = pagerank::compute_weighted_pagerank(
            &mut self.symbols,
            &self.callers_map,
            &self.callees_map,
            &self.edge_weights,
            Some(indices),
        );
        self.pagerank_iters = pr.iters;
        self.pagerank_converged = pr.converged;
    }

    pub fn compute_personalized_pagerank(&mut self, focus: &Coordinate) {
        let focus_indices = self.resolve_all(focus);
        let pr = pagerank::compute_weighted_pagerank(
            &mut self.symbols,
            &self.callers_map,
            &self.callees_map,
            &self.edge_weights,
            Some(&focus_indices),
        );
        self.pagerank_iters = pr.iters;
        self.pagerank_converged = pr.converged;
    }

    pub fn format_ambiguous_error(
        &self,
        coord: &Coordinate,
        indices: &[usize],
        verb: &str,
    ) -> String {
        let mut matches: Vec<&Symbol> = indices.iter().map(|&i| &self.symbols[i]).collect();
        matches.sort_by(|a, b| {
            b.centrality
                .total_cmp(&a.centrality)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        let coords: Vec<String> = matches
            .iter()
            .take(15)
            .map(|s| {
                format!(
                    "  - `{}` ({}) [rank: {:.4}]",
                    s.coordinate(),
                    s.kind.as_str(),
                    s.centrality
                )
            })
            .collect();
        let retries: Vec<String> = matches
            .iter()
            .take(5)
            .map(|s| format!("  mimori {} '{}'", verb, s.coordinate()))
            .collect();
        format!(
            "Ambiguous symbol '{}'. Multiple matches found, please specify full coordinate:\n{}\nRetry with one of:\n{}",
            coord,
            coords.join("\n"),
            retries.join("\n")
        )
    }

    /// Resolve strictly to a unique symbol index, failing closed on NotFound or Ambiguous.
    pub fn resolve_strict(&self, coord: &Coordinate, verb: &str) -> Result<usize> {
        match self.resolve(coord) {
            Resolution::Unique(idx) => Ok(idx),
            Resolution::NotFound => bail!("{}", self.format_not_found_error(coord)),
            Resolution::Ambiguous(indices) => {
                bail!("{}", self.format_ambiguous_error(coord, &indices, verb))
            }
        }
    }

    /// Strict upstream resolution: resolves `coord` uniquely, then folds in
    /// class/struct constructors (`::constructor`, `.constructor`, `::new`, `::__init__`).
    pub fn resolve_strict_upstream(&self, coord: &Coordinate, verb: &str) -> Result<Vec<usize>> {
        let idx = self.resolve_strict(coord, verb)?;
        Ok(self.expand_upstream_indices(&[idx]))
    }

    pub fn expand_upstream_indices(&self, base: &[usize]) -> Vec<usize> {
        let mut indices = Vec::with_capacity(base.len());
        let mut seen = HashSet::new();
        for &idx in base {
            if seen.insert(idx) {
                indices.push(idx);
            }
            let sym = &self.symbols[idx];
            if matches!(sym.kind, SymbolKind::Class | SymbolKind::Struct) {
                for suffix in ["::constructor", ".constructor", "::new", "::__init__"] {
                    let ctor = format!("{}{}", sym.name, suffix);
                    if let Some(more) = self.name_to_indices.get(&ctor) {
                        for &e in more {
                            if self.symbols[e].file == sym.file && seen.insert(e) {
                                indices.push(e);
                            }
                        }
                    }
                }
            }
        }
        indices
    }

    pub fn mentioners(&self, coord: &Coordinate) -> Vec<&Symbol> {
        let resolved = self.resolve_all(coord);
        let mut seen = HashSet::new();
        let mut indices = Vec::new();
        if !resolved.is_empty() {
            for idx in resolved {
                for m_idx in self.mentioner_indices(idx) {
                    if seen.insert(m_idx) {
                        indices.push(m_idx);
                    }
                }
            }
        } else {
            let Some(name) = coord.name() else {
                return Vec::new();
            };
            let short = name.rsplit([':', '.']).next().unwrap_or(name);
            for key in [name, short] {
                if let Some(list) = self.mention_to_indices.get(key) {
                    for &m_idx in list {
                        if seen.insert(m_idx) {
                            indices.push(m_idx);
                        }
                    }
                }
            }
        }
        indices.sort_unstable();
        indices.into_iter().map(|idx| &self.symbols[idx]).collect()
    }

    pub fn mentioners_strict(&self, coord: &Coordinate) -> Result<Vec<&Symbol>> {
        match self.resolve(coord) {
            Resolution::Unique(idx) => Ok(self
                .mentioner_indices(idx)
                .into_iter()
                .map(|i| &self.symbols[i])
                .collect()),
            Resolution::Ambiguous(indices) => {
                bail!("{}", self.format_ambiguous_error(coord, &indices, "uses"))
            }
            Resolution::NotFound => {
                if coord.file().is_none() {
                    let hits = self.mentioners(coord);
                    if !hits.is_empty() {
                        return Ok(hits);
                    }
                }
                bail!("{}", self.format_not_found_error(coord))
            }
        }
    }

    pub fn mentioner_indices(&self, target_idx: usize) -> Vec<usize> {
        let name = self.symbols[target_idx].name.as_str();
        let short = name.rsplit([':', '.']).next().unwrap_or(name);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for key in [name, short] {
            if let Some(list) = self.mention_to_indices.get(key) {
                for &i in list {
                    if i != target_idx && seen.insert(i) {
                        out.push(i);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    pub fn upstream(&self, coord: &Coordinate) -> (Vec<&Symbol>, Vec<&Symbol>) {
        let caller_indices = self.caller_index_set(coord);
        let callers: Vec<&Symbol> = caller_indices.iter().map(|&i| &self.symbols[i]).collect();
        let seen_set: HashSet<usize> = caller_indices.into_iter().collect();
        let resolved = self.resolve_all(coord);
        let mut weak_indices = Vec::new();
        let mut weak_seen = HashSet::new();
        if !resolved.is_empty() {
            for idx in resolved {
                for m_idx in self.mentioner_indices(idx) {
                    if !seen_set.contains(&m_idx) && weak_seen.insert(m_idx) {
                        weak_indices.push(m_idx);
                    }
                }
            }
        } else if let Some(name) = coord.name() {
            let short = name.rsplit([':', '.']).next().unwrap_or(name);
            for key in [name, short] {
                if let Some(list) = self.mention_to_indices.get(key) {
                    for &m_idx in list {
                        if !seen_set.contains(&m_idx) && weak_seen.insert(m_idx) {
                            weak_indices.push(m_idx);
                        }
                    }
                }
            }
        }
        weak_indices.sort_unstable();
        let weak = weak_indices.into_iter().map(|i| &self.symbols[i]).collect();
        (callers, weak)
    }

    pub fn upstream_strict(&self, coord: &Coordinate) -> Result<(Vec<&Symbol>, Vec<&Symbol>)> {
        let target_indices = self.resolve_strict_upstream(coord, "up")?;
        let mut caller_indices = Vec::new();
        let mut seen_callers = HashSet::new();
        for &t in &target_indices {
            if let Some(callers) = self.callers_map.get(&t) {
                for &c in callers {
                    if seen_callers.insert(c) {
                        caller_indices.push(c);
                    }
                }
            }
        }
        caller_indices.sort_unstable();

        let mut weak_indices = Vec::new();
        let mut weak_seen = HashSet::new();
        for &t in &target_indices {
            for m_idx in self.mentioner_indices(t) {
                if !seen_callers.contains(&m_idx) && weak_seen.insert(m_idx) {
                    weak_indices.push(m_idx);
                }
            }
        }
        weak_indices.sort_unstable();

        Ok((
            caller_indices
                .into_iter()
                .map(|i| &self.symbols[i])
                .collect(),
            weak_indices.into_iter().map(|i| &self.symbols[i]).collect(),
        ))
    }

    pub fn callees_strict(&self, coord: &Coordinate) -> Result<Vec<&Symbol>> {
        let idx = self.resolve_strict(coord, "down")?;
        let mut out = self.callees_map.get(&idx).cloned().unwrap_or_default();
        out.sort_unstable();
        out.dedup();
        Ok(out.into_iter().map(|i| &self.symbols[i]).collect())
    }

    pub fn resolve_all(&self, coord: &Coordinate) -> Vec<usize> {
        match self.resolve(coord) {
            Resolution::Unique(idx) => vec![idx],
            Resolution::Ambiguous(indices) => indices,
            Resolution::NotFound => Vec::new(),
        }
    }

    pub fn resolve(&self, coord: &Coordinate) -> Resolution {
        let Some(name) = coord.name() else {
            return Resolution::NotFound;
        };

        let Some(by_name) = self.name_to_indices.get(name) else {
            return Resolution::NotFound;
        };
        if by_name.is_empty() {
            return Resolution::NotFound;
        }

        let Some(target_file) = coord.file() else {
            return Resolution::from(by_name.clone());
        };

        let clean_target_str = crate::workspace::walker::normalize_rel_str(target_file);
        let clean_target = Path::new(
            clean_target_str
                .strip_prefix("./")
                .unwrap_or(&clean_target_str),
        );

        let exact_hits: Vec<usize> = by_name
            .iter()
            .copied()
            .filter(|&idx| Path::new(&self.symbols[idx].file) == clean_target)
            .collect();
        if !exact_hits.is_empty() {
            return Resolution::from(exact_hits);
        }

        let suffix_hits: Vec<usize> = by_name
            .iter()
            .copied()
            .filter(|&idx| Path::new(&self.symbols[idx].file).ends_with(clean_target))
            .collect();
        if !suffix_hits.is_empty() {
            return Resolution::from(suffix_hits);
        }

        Resolution::NotFound
    }

    pub fn resolve_upstream_targets(&self, coord: &Coordinate) -> Vec<usize> {
        let base = self.resolve_all(coord);
        self.expand_upstream_indices(&base)
    }

    fn caller_index_set(&self, coord: &Coordinate) -> Vec<usize> {
        let indices = self.resolve_upstream_targets(coord);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for target_idx in indices {
            if let Some(callers) = self.callers_map.get(&target_idx) {
                for &caller_idx in callers {
                    if seen.insert(caller_idx) {
                        out.push(caller_idx);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    pub fn callers(&self, coord: &Coordinate) -> Vec<&Symbol> {
        self.caller_index_set(coord)
            .into_iter()
            .map(|i| &self.symbols[i])
            .collect()
    }

    pub fn callees(&self, coord: &Coordinate) -> Vec<&Symbol> {
        let indices = self.resolve_all(coord);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for target_idx in indices {
            if let Some(callees) = self.callees_map.get(&target_idx) {
                for &callee_idx in callees {
                    if seen.insert(callee_idx) {
                        out.push(callee_idx);
                    }
                }
            }
        }
        out.sort_unstable();
        out.into_iter().map(|i| &self.symbols[i]).collect()
    }

    pub fn read_symbol_body(&self, sym: &Symbol) -> Result<String> {
        if !sym.body.is_empty() {
            return Ok(sym.body.clone());
        }
        let full_path = match &self.root {
            Some(root) => {
                crate::workspace::walker::confine_to_workspace(root, Path::new(&sym.file))?
            }
            None => PathBuf::from(&sym.file),
        };
        let bytes = fs::read(&full_path)
            .with_context(|| format!("Failed to read source file '{}'", sym.file))?;
        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() || sym.start_line == 0 || sym.start_line > lines.len() {
            return Ok(String::new());
        }
        let start_idx = sym.start_line - 1;
        let end_idx = sym.end_line.min(lines.len());
        if start_idx >= end_idx {
            return Ok(String::new());
        }
        Ok(lines[start_idx..end_idx].join("\n"))
    }

    pub fn build_slice(
        &self,
        coord: &Coordinate,
        follow_local: bool,
        with_imports: bool,
    ) -> Result<SliceResult> {
        if let Coordinate::Lines { file, start, end } = coord {
            let resolved = match &self.root {
                Some(root) => crate::workspace::walker::confine_to_workspace(root, file)?,
                None => file.clone(),
            };
            return slice_line_coordinate(
                &resolved,
                *start,
                *end,
                with_imports,
                self.root.as_deref(),
            );
        }

        let target_idx = self.resolve_strict(coord, "slice")?;
        let sym = &self.symbols[target_idx];

        let callers: Vec<String> = self
            .callers(coord)
            .into_iter()
            .map(|s| format!("{} (L{}-L{})", s.coordinate(), s.start_line, s.end_line))
            .collect();

        let callee_syms = self.callees(coord);
        let callees: Vec<String> = callee_syms
            .iter()
            .map(|s| format!("{} (L{}-L{})", s.coordinate(), s.start_line, s.end_line))
            .collect();

        let raw_sym_body = self.read_symbol_body(sym)?;
        let span = sym.end_line.saturating_sub(sym.start_line);
        let body = truncate_body(&raw_sym_body, span);

        let mut local_callees = Vec::new();
        if follow_local {
            for lc in callee_syms
                .iter()
                .filter(|c| c.file == sym.file)
                .take(MAX_INLINED_LOCALS)
            {
                let lc_raw = self.read_symbol_body(lc).unwrap_or_default();
                let lc_span = lc.end_line.saturating_sub(lc.start_line);
                local_callees.push(InlinedCallee {
                    name: lc.name.clone(),
                    start_line: lc.start_line,
                    end_line: lc.end_line,
                    content: truncate_body(&lc_raw, lc_span),
                });
            }
        }

        let imports = if with_imports {
            let import_path = match &self.root {
                Some(root) => {
                    crate::workspace::walker::confine_to_workspace(root, Path::new(&sym.file)).ok()
                }
                None => Some(PathBuf::from(&sym.file)),
            };
            let imps = import_path
                .as_deref()
                .map(extract_file_imports)
                .unwrap_or_default();
            (!imps.is_empty()).then_some(imps)
        } else {
            None
        };

        let memory_notes = self
            .root
            .as_deref()
            .map(|r| {
                crate::memory::MemoryLedger::scoped_notes_for_workspace(
                    r,
                    &[sym.file.as_str()],
                    &[sym.name.as_str()],
                )
            })
            .unwrap_or_default();

        let mut sym_meta = sym.clone();
        sym_meta.body.clear();

        Ok(SliceResult {
            coordinate: sym.coordinate(),
            file: sym.file.clone(),
            symbol: Some(sym_meta),
            line_range: Some((sym.start_line, sym.end_line)),
            content: body,
            callers,
            callees,
            total_lines: span + 1,
            imports,
            local_callees,
            memory_notes,
        })
    }

    pub fn format_not_found_error(&self, coord: &Coordinate) -> String {
        let bare = coord.name().unwrap_or("");
        let mut msg = format!("Symbol '{}' not found in workspace.", coord);
        if let Some(target_file) = coord.file() {
            let clean_target_str = crate::workspace::walker::normalize_rel_str(target_file);
            let clean_target = Path::new(
                clean_target_str
                    .strip_prefix("./")
                    .unwrap_or(&clean_target_str),
            );
            let mut file_syms: Vec<&Symbol> = self
                .symbols
                .iter()
                .filter(|s| Path::new(&s.file) == clean_target)
                .collect();
            if file_syms.is_empty() {
                file_syms = self
                    .symbols
                    .iter()
                    .filter(|s| Path::new(&s.file).ends_with(clean_target))
                    .collect();
            }
            if !file_syms.is_empty() {
                let q = bare.to_lowercase();
                file_syms.sort_by(|a, b| {
                    let a_sub =
                        a.name.to_lowercase().contains(&q) || q.contains(&a.name.to_lowercase());
                    let b_sub =
                        b.name.to_lowercase().contains(&q) || q.contains(&b.name.to_lowercase());
                    b_sub
                        .cmp(&a_sub)
                        .then_with(|| a.start_line.cmp(&b.start_line))
                });
                let matched_file = &file_syms[0].file;
                msg.push_str(&format!("\nSymbols defined in '{}':", matched_file));
                for s in file_syms.iter().take(10) {
                    msg.push_str(&format!(
                        "\n  - {} ({}, L{}-L{})",
                        s.coordinate(),
                        s.kind.as_str(),
                        s.start_line,
                        s.end_line
                    ));
                }
            }
        }
        let find_arg = if bare.is_empty() {
            coord.to_string()
        } else {
            bare.to_string()
        };
        msg.push_str(&format!("\nTry: mimori find '{}'", find_arg));
        msg
    }
}

fn module_hint_matches(sym_file: &str, hint: &str) -> bool {
    let hint_clean = hint
        .trim_start_matches("./")
        .trim_start_matches("../")
        .trim_matches('/');
    if hint_clean.is_empty() {
        return false;
    }
    let sym_no_ext = Path::new(sym_file).with_extension("");
    let sym_stem_str = sym_no_ext.to_string_lossy();
    let sym_trimmed = sym_stem_str
        .strip_suffix("/index")
        .or_else(|| sym_stem_str.strip_suffix("/mod"))
        .unwrap_or(&sym_stem_str);
    sym_trimmed == hint_clean
        || component_suffix_match(Path::new(sym_trimmed), Path::new(hint_clean))
        || component_suffix_match(Path::new(sym_file), Path::new(hint_clean))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Unique(usize),
    Ambiguous(Vec<usize>),
    NotFound,
}

impl From<Vec<usize>> for Resolution {
    fn from(mut hits: Vec<usize>) -> Self {
        match hits.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Unique(hits.remove(0)),
            _ => Resolution::Ambiguous(hits),
        }
    }
}

/// True when `b` is a component-aligned suffix of `a` (`a.len() >= b.len()`).
fn component_suffix_match(a: &Path, b: &Path) -> bool {
    let av: Vec<_> = a.components().collect();
    let bv: Vec<_> = b.components().collect();
    !bv.is_empty() && av.len() >= bv.len() && av[av.len() - bv.len()..] == bv[..]
}

fn truncate_body(body: &str, span: usize) -> String {
    const LIMIT: usize = 250;
    const HEAD: usize = 200;
    const TAIL: usize = 30;

    let lines: Vec<&str> = body.lines().collect();
    if span <= LIMIT || lines.len() <= HEAD + TAIL {
        return body.to_string();
    }

    format!(
        "{}\n\n// ... [{} lines truncated for token efficiency] ...\n\n{}",
        lines[..HEAD].join("\n"),
        lines.len() - HEAD - TAIL,
        lines[lines.len() - TAIL..].join("\n")
    )
}

pub fn extract_file_imports(file_path: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(file_path) else {
        return Vec::new();
    };

    let mut imports = Vec::new();
    let mut in_multiline_import = false;
    let mut multiline_buf = String::new();

    for line in content.lines().take(400) {
        let trimmed = line.trim();
        if !in_multiline_import && (trimmed.starts_with("#[cfg(test)]") || trimmed == "mod tests {")
        {
            break;
        }

        if in_multiline_import {
            multiline_buf.push_str(line);
            multiline_buf.push('\n');
            let expects_paren = multiline_buf.contains('(') || multiline_buf.contains('{');
            if trimmed.contains(')')
                || trimmed.contains('}')
                || trimmed.ends_with(';')
                || (!expects_paren && !trimmed.ends_with(',') && !trimmed.ends_with('\\'))
            {
                in_multiline_import = false;
                imports.push(multiline_buf.trim_end().to_string());
                multiline_buf.clear();
            }
            continue;
        }

        if trimmed.starts_with("use ")
            || trimmed.starts_with("pub use ")
            || trimmed.starts_with("extern crate ")
        {
            if trimmed.ends_with(';') {
                imports.push(trimmed.to_string());
            } else {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            }
        } else if trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("import type ")
            || trimmed.starts_with("import (")
            || trimmed.starts_with("import(")
        {
            if trimmed.ends_with(';')
                || trimmed.ends_with('\'')
                || trimmed.ends_with('"')
                || (!trimmed.contains('{')
                    && !trimmed.contains('(')
                    && !trimmed.ends_with(',')
                    && !trimmed.ends_with('\\'))
            {
                imports.push(trimmed.to_string());
            } else {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            }
        } else if trimmed.starts_with("from ") && trimmed.contains(" import ") {
            if trimmed.ends_with('\\') || (trimmed.contains('(') && !trimmed.contains(')')) {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            } else {
                imports.push(trimmed.to_string());
            }
        } else if (trimmed.starts_with("const ") || trimmed.starts_with("let "))
            && trimmed.contains("= require(")
        {
            imports.push(trimmed.to_string());
        }
    }

    imports
}

/// Slice a line range straight off disk. Relativizes display paths against
/// `workspace_root` when provided so absolute host paths never leak into output.
pub fn slice_line_coordinate(
    file: &Path,
    start: usize,
    end: usize,
    with_imports: bool,
    workspace_root: Option<&Path>,
) -> Result<SliceResult> {
    if start == 0 || end == 0 {
        bail!("Line numbers are 1-based; line 0 is invalid.");
    }
    if !file.exists() {
        bail!("File not found: {}", file.display());
    }

    let content = fs::read_to_string(file)
        .with_context(|| format!("Failed to read file: {}", file.display()))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let (start, end) = if start > end {
        (end, start)
    } else {
        (start, end)
    };
    if start > total_lines {
        bail!(
            "Line {} is past the end of {} ({} lines).",
            start,
            file.display(),
            total_lines
        );
    }

    let start_idx = start - 1;
    let end_clamped = end.min(total_lines).max(start);

    let mut sliced = String::new();
    for (i, line) in lines[start_idx..end_clamped].iter().enumerate() {
        sliced.push_str(&format!("{:4} | {}\n", start + i, line));
    }

    let imports = if with_imports {
        let imps = extract_file_imports(file);
        (!imps.is_empty()).then_some(imps)
    } else {
        None
    };

    let display_file = match workspace_root {
        Some(root) => {
            let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            let canon_file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
            canon_file
                .strip_prefix(&canon_root)
                .map(crate::workspace::walker::normalize_rel_str)
                .unwrap_or_else(|_| crate::workspace::walker::normalize_rel_str(file))
        }
        None => crate::workspace::walker::normalize_rel_str(file),
    };

    let memory_notes = workspace_root
        .map(|r| {
            crate::memory::MemoryLedger::scoped_notes_for_workspace(
                r,
                &[display_file.as_str()],
                &[],
            )
        })
        .unwrap_or_default();

    Ok(SliceResult {
        coordinate: format!("{}:#L{}-{}", display_file, start, end_clamped),
        file: display_file,
        symbol: None,
        line_range: Some((start, end_clamped)),
        content: sliced,
        callers: Vec::new(),
        callees: Vec::new(),
        total_lines: end_clamped - start_idx,
        imports,
        local_callees: Vec::new(),
        memory_notes,
    })
}

pub fn zero_caller_hint(graph: &SymbolGraph, coord: &Coordinate) -> Option<String> {
    let name = coord.name()?;
    if name.contains("::") || name.contains('.') {
        return None;
    }
    let has_ctor = ["::constructor", "::new"].iter().any(|sfx| {
        graph
            .name_to_indices
            .contains_key(&format!("{}{}", name, sfx))
    });
    let is_type = graph.resolve_all(coord).iter().any(|&i| {
        matches!(
            graph.symbols[i].kind.as_str(),
            "class" | "struct" | "interface" | "trait"
        )
    });
    if has_ctor || is_type {
        Some(format!(
            "Hint: 0 callers — if `{0}` is a class/struct, construction sites are `new {0}(...)` edges: try `mimori up {0}::constructor`, and confirm with `rg -n \"new {0}\"`.",
            name
        ))
    } else {
        None
    }
}

pub fn format_upstream(
    graph: &SymbolGraph,
    coord: &Coordinate,
    callers: &[&Symbol],
    value_uses: &[&Symbol],
) -> String {
    let mut out = format!(
        "### Upstream Callers: `{}` ({} callers)\n\n{}\n\n",
        coord,
        callers.len(),
        COUNT_LEGEND
    );
    if callers.is_empty() && value_uses.is_empty() {
        out.push_str("No upstream callers found.\n");
        if let Some(hint) = zero_caller_hint(graph, coord) {
            out.push_str(&hint);
            out.push('\n');
        }
    } else {
        for c in callers {
            out.push_str(&format!(
                "- **`{}`** ({}) -> `{}` (L{}-L{})\n",
                c.name,
                c.kind.as_str(),
                c.coordinate(),
                c.start_line,
                c.end_line
            ));
        }
        if !value_uses.is_empty() {
            out.push_str("\n### Value Uses (mentions — weakest tier, not call edges)\n\n");
            for m in value_uses {
                out.push_str(&format!(
                    "- **`{}`** ({}) -> `{}` (L{}-L{}) [Value Use]\n",
                    m.name,
                    m.kind.as_str(),
                    m.coordinate(),
                    m.start_line,
                    m.end_line
                ));
            }
        }
    }
    out
}

pub fn format_downstream(_graph: &SymbolGraph, coord: &Coordinate, callees: &[&Symbol]) -> String {
    let mut out = format!(
        "### Downstream Callees: `{}` ({} callees)\n\n{}\n\n",
        coord,
        callees.len(),
        COUNT_LEGEND
    );
    if callees.is_empty() {
        out.push_str("No downstream callees found.\n");
    } else {
        for c in callees {
            out.push_str(&format!(
                "- **`{}`** ({}) -> `{}` (L{}-L{})\n",
                c.name,
                c.kind.as_str(),
                c.coordinate(),
                c.start_line,
                c.end_line
            ));
        }
    }
    out
}

pub fn format_uses(_graph: &SymbolGraph, coord: &Coordinate, mentioners: &[&Symbol]) -> String {
    let mut out = format!(
        "### Mentioners: `{}` ({} mentioners, non-call uses)\n\n{}\n\n",
        coord,
        mentioners.len(),
        COUNT_LEGEND
    );
    if mentioners.is_empty() {
        out.push_str("No mentioners found.\n");
    } else {
        for m in mentioners {
            out.push_str(&format!(
                "- **`{}`** ({}) -> `{}` (L{}-L{})\n",
                m.name,
                m.kind.as_str(),
                m.coordinate(),
                m.start_line,
                m.end_line
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use std::io::Write;

    fn sym(file: &str, name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 1,
            signature: String::new(),
            body: format!("fn {name}() {{ /* {file} */ }}"),
            centrality: 0.0,
            calls: vec![],
            mentions: vec![],
            call_counts: Default::default(),
            member_calls: vec![],
            external_imports: crate::model::symbol::empty_external_imports(),
            flags: 0,
        }
    }

    fn graph_of(files: &[(&str, &str)]) -> SymbolGraph {
        SymbolGraph::new(files.iter().map(|(f, n)| sym(f, n)).collect())
    }

    fn at(g: &SymbolGraph, raw: &str) -> Resolution {
        g.resolve(&Coordinate::parse(raw).unwrap())
    }

    #[test]
    fn exact_path_beats_a_basename_collision() {
        let g = graph_of(&[
            ("src/alpha/mod.rs", "handler"),
            ("src/beta/mod.rs", "handler"),
        ]);
        let Resolution::Unique(i) = at(&g, "src/alpha/mod.rs:handler") else {
            panic!("exact path must resolve uniquely");
        };
        assert_eq!(g.symbols[i].file, "src/alpha/mod.rs");
        let Resolution::Unique(j) = at(&g, "src/beta/mod.rs:handler") else {
            panic!("exact path must resolve uniquely");
        };
        assert_eq!(g.symbols[j].file, "src/beta/mod.rs");
    }

    #[test]
    fn a_basename_collision_is_ambiguous_not_a_guess() {
        let g = graph_of(&[
            ("src/alpha/mod.rs", "handler"),
            ("src/beta/mod.rs", "handler"),
        ]);
        assert!(matches!(at(&g, "mod.rs:handler"), Resolution::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn a_unique_basename_still_resolves() {
        let g = graph_of(&[("src/auth_service.rs", "login"), ("src/other.rs", "logout")]);
        assert!(matches!(
            at(&g, "auth_service.rs:login"),
            Resolution::Unique(_)
        ));
    }

    #[test]
    fn a_path_suffix_resolves_on_component_boundaries() {
        let g = graph_of(&[
            ("src/alpha/mod.rs", "handler"),
            ("src/beta/mod.rs", "handler"),
        ]);
        assert!(matches!(
            at(&g, "alpha/mod.rs:handler"),
            Resolution::Unique(_)
        ));
        assert!(matches!(at(&g, "ha/mod.rs:handler"), Resolution::NotFound));
    }

    #[test]
    fn component_suffix_is_asymmetric_and_component_aligned() {
        assert!(component_suffix_match(
            Path::new("src/alpha/mod.rs"),
            Path::new("alpha/mod.rs")
        ));
        assert!(!component_suffix_match(
            Path::new("src/alpha/mod.rs"),
            Path::new("ha/mod.rs")
        ));
        assert!(!component_suffix_match(
            Path::new("mod.rs"),
            Path::new("src/alpha/mod.rs")
        ));
    }

    #[test]
    fn qualified_bare_names_resolve_through_the_name_tier() {
        let g = graph_of(&[("src/lib.rs", "Store::save"), ("src/lib.rs", "other")]);
        assert!(matches!(at(&g, "Store::save"), Resolution::Unique(_)));
        assert!(matches!(at(&g, "save"), Resolution::Unique(_)));
    }

    fn sym_with_refs(file: &str, name: &str, refs: &[&str]) -> Symbol {
        let mut s = sym(file, name);
        s.calls = refs.iter().map(|r| r.to_string()).collect();
        for r in refs {
            s.call_counts.insert(r.to_string(), 1);
        }
        s
    }

    fn caller(
        file: &str,
        name: &str,
        calls: &[&str],
        member: &[&str],
        external: &[&str],
    ) -> Symbol {
        let mut s = sym_with_refs(file, name, calls);
        s.member_calls = member.iter().map(|r| r.to_string()).collect();
        s.external_imports = external
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .into();
        s
    }

    fn method(file: &str, name: &str) -> Symbol {
        let mut s = sym(file, name);
        s.kind = SymbolKind::Method;
        s
    }

    fn variable(file: &str, name: &str) -> Symbol {
        let mut s = sym(file, name);
        s.kind = SymbolKind::Variable;
        s
    }

    #[test]
    fn a_reference_prefers_a_match_in_its_own_file() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["target"]),
            sym("a.rs", "target"),
            sym("b.rs", "target"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].file, "a.rs");
    }

    #[test]
    fn a_unique_name_links_across_files() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["only_one"]),
            sym("b.rs", "only_one"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert_eq!(callees.len(), 1);
    }

    #[test]
    fn prod_calls_never_bind_to_test_helpers_across_files() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("src/main.rs", "main", &["write"]),
            sym("tests/cli_path_aliases.rs", "write"),
        ]);
        assert!(g
            .callees(&Coordinate::parse("src/main.rs:main").unwrap())
            .is_empty());
    }

    #[test]
    fn generic_member_calls_do_not_hijack_same_file_methods() {
        let g = SymbolGraph::new(vec![
            caller(
                "src/aliases.rs",
                "collect",
                &["is_empty"],
                &["is_empty"],
                &[],
            ),
            method("src/aliases.rs", "AliasSet::is_empty"),
        ]);
        assert!(g
            .callers(&Coordinate::parse("src/aliases.rs:AliasSet::is_empty").unwrap())
            .is_empty());
    }

    #[test]
    fn an_ambiguous_name_links_to_nothing_rather_than_everything() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["new"]),
            sym("b.rs", "new"),
            sym("c.rs", "new"),
            sym("d.rs", "new"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert!(callees.is_empty());
    }

    #[test]
    fn mentions_never_become_call_edges() {
        fn reader(file: &str, name: &str) -> Symbol {
            let mut s = sym(file, name);
            s.mentions = vec!["serverId".to_string()];
            s
        }
        fn field(file: &str) -> Symbol {
            let mut s = sym(file, "Store::serverId");
            s.kind = SymbolKind::Field;
            s.body.clear();
            s
        }
        let mut syms = vec![field("a.rs"), field("b.rs"), field("c.rs")];
        for i in 0..12 {
            syms.push(reader(&format!("r{i}.rs"), &format!("reader{i}")));
        }
        let g = SymbolGraph::new(syms);
        for f in ["a.rs", "b.rs", "c.rs"] {
            let coord = Coordinate::parse(&format!("{f}:serverId")).unwrap();
            assert!(g.callers(&coord).is_empty());
        }
        let coord = Coordinate::parse("serverId").unwrap();
        assert_eq!(g.mentioners(&coord).len(), 12);
    }

    #[test]
    fn upstream_partitions_callers_from_value_uses_with_call_wins() {
        let mut both = sym_with_refs("a.rs", "both", &["target"]);
        both.mentions = vec!["target".to_string()];
        let mut pure = sym("a.rs", "pure");
        pure.mentions = vec!["target".to_string()];
        let g = SymbolGraph::new(vec![both, pure, sym("b.rs", "target")]);
        let coord = Coordinate::parse("target").unwrap();
        let (callers, weak) = g.upstream(&coord);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].name, "both");
        assert_eq!(weak.len(), 1);
        assert_eq!(weak[0].name, "pure");
    }

    #[test]
    fn fields_vanish_from_top_ranks() {
        fn reader_of(name: &str, calls: &[&str], mentions: &[&str]) -> Symbol {
            let mut s = sym("r.rs", name);
            s.calls = calls.iter().map(|r| r.to_string()).collect();
            for c in calls {
                s.call_counts.insert(c.to_string(), 1);
            }
            s.mentions = mentions.iter().map(|r| r.to_string()).collect();
            s
        }
        let mut field = sym("s.rs", "Store::input");
        field.kind = SymbolKind::Field;
        field.body.clear();
        let mut syms = vec![sym("hub.rs", "realHub"), field];
        for i in 0..10 {
            syms.push(reader_of(&format!("caller{i}"), &["realHub"], &["input"]));
        }
        let g = SymbolGraph::new(syms);
        let mut ranked = g.symbols.clone();
        ranked.sort_by(|a, b| b.centrality.total_cmp(&a.centrality));
        assert_eq!(ranked[0].name, "realHub");
        let hub_c = ranked
            .iter()
            .find(|s| s.name == "realHub")
            .unwrap()
            .centrality;
        let field_c = ranked
            .iter()
            .find(|s| s.name == "Store::input")
            .unwrap()
            .centrality;
        assert!(hub_c > field_c);
    }

    #[test]
    fn resolver_counts_what_it_drops() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["new", "missing", "only_one"]),
            sym("b.rs", "new"),
            sym("c.rs", "new"),
            sym("d.rs", "only_one"),
        ]);
        assert_eq!(g.resolve_stats.resolved, 1);
        assert_eq!(g.resolve_stats.ambiguous_dropped, 1);
        assert_eq!(g.resolve_stats.unresolved, 1);
    }

    #[test]
    fn same_file_ambiguity_drops_instead_of_picking_index_order() {
        let mut m_target = sym("a.rs", "M::target");
        m_target.start_line = 9;
        m_target.end_line = 9;
        m_target.body.clear();
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["target"]),
            sym("a.rs", "target"),
            m_target,
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert!(callees.is_empty());
        assert_eq!(g.resolve_stats.ambiguous_dropped, 1);
    }

    #[test]
    fn same_directory_beats_unique_global() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("src/a/caller.rs", "caller", &["helper"]),
            sym("src/a/helper.rs", "helper"),
            sym("src/b/helper.rs", "helper"),
        ]);
        let callees = g.callees(&Coordinate::parse("src/a/caller.rs:caller").unwrap());
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].file, "src/a/helper.rs");
    }

    #[test]
    fn pagerank_converges_and_reports() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "x", &["y"]),
            sym_with_refs("a.rs", "y", &["x"]),
        ]);
        assert!(g.pagerank_converged);
        assert!(g.pagerank_iters >= 1 && g.pagerank_iters <= 100);
    }

    #[test]
    fn edge_weights_scale_sublinearly_with_multiplicity() {
        assert!((multiplicity_weight(1) - 1.0).abs() < 1e-9);
        assert!((multiplicity_weight(50) - 50f64.sqrt()).abs() < 1e-9);
        assert!((multiplicity_weight(1000) - 8.0).abs() < 1e-9);
        let mut heavy = sym_with_refs("a.rs", "caller", &["hot", "cold"]);
        heavy.call_counts.insert("hot".to_string(), 50);
        heavy.call_counts.insert("cold".to_string(), 1);
        let g = SymbolGraph::new(vec![heavy, sym("b.rs", "hot"), sym("c.rs", "cold")]);
        let hot_w = g.edge_weights.get(&(0, 1)).copied().unwrap_or(0.0);
        let cold_w = g.edge_weights.get(&(0, 2)).copied().unwrap_or(0.0);
        assert!(hot_w > cold_w);
    }

    #[test]
    fn graph_construction_is_deterministic() {
        fn build() -> (Vec<(usize, usize)>, Vec<f64>) {
            let g = SymbolGraph::new(vec![
                sym_with_refs("a.rs", "caller", &["target", "other"]),
                sym("a.rs", "target"),
                sym("b.rs", "other"),
            ]);
            let mut edges: Vec<(usize, usize)> = g
                .callees_map
                .iter()
                .flat_map(|(u, vs)| vs.iter().map(|v| (*u, *v)))
                .collect();
            edges.sort();
            let ranks: Vec<f64> = g.symbols.iter().map(|s| s.centrality).collect();
            (edges, ranks)
        }
        assert_eq!(build(), build());
    }

    #[test]
    fn an_unknown_symbol_is_not_found() {
        let g = graph_of(&[("src/lib.rs", "handler")]);
        assert!(matches!(at(&g, "nope"), Resolution::NotFound));
        assert!(matches!(at(&g, "src/lib.rs:nope"), Resolution::NotFound));
    }

    #[test]
    fn build_slice_refuses_to_guess_between_ambiguous_matches() {
        let g = graph_of(&[
            ("src/alpha/mod.rs", "handler"),
            ("src/beta/mod.rs", "handler"),
        ]);
        let err = g
            .build_slice(&Coordinate::parse("mod.rs:handler").unwrap(), false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Ambiguous"), "got: {err}");
        assert!(err.contains("mimori slice 'src/alpha/mod.rs:handler'"));
    }

    fn fixture(lines: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 1..=lines {
            writeln!(f, "line {i}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn line_zero_is_rejected_cleanly() {
        let f = fixture(10);
        assert!(slice_line_coordinate(f.path(), 0, 5, false, None).is_err());
    }

    #[test]
    fn reversed_ranges_are_normalized() {
        let f = fixture(10);
        let res = slice_line_coordinate(f.path(), 8, 2, false, None).unwrap();
        assert_eq!(res.line_range, Some((2, 8)));
        assert!(res.content.contains("line 2") && res.content.contains("line 8"));
    }

    #[test]
    fn start_past_end_of_file_is_an_error() {
        let f = fixture(10);
        let err = slice_line_coordinate(f.path(), 400, 500, false, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("past the end"), "got: {err}");
    }

    #[test]
    fn end_past_end_of_file_clamps() {
        let f = fixture(10);
        let res = slice_line_coordinate(f.path(), 8, 500, false, None).unwrap();
        assert_eq!(res.line_range, Some((8, 10)));
        assert!(res.content.contains("line 10"));
    }

    #[test]
    fn member_calls_resolve_same_file_only_and_record_unresolved() {
        let g = SymbolGraph::new(vec![
            caller("a.ts", "localUser", &["input"], &["input"], &[]),
            method("a.ts", "input"),
            method("b.ts", "input"),
            caller("c.ts", "remoteUser", &["input"], &["input"], &[]),
        ]);
        let local = g.callees(&Coordinate::parse("a.ts:localUser").unwrap());
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].file, "a.ts");
        let remote = g.callees(&Coordinate::parse("c.ts:remoteUser").unwrap());
        assert!(remote.is_empty());
        assert_eq!(g.unresolved_member_calls.get("input"), Some(&1));
    }

    #[test]
    fn bare_calls_still_resolve_unique_global() {
        let g = SymbolGraph::new(vec![
            caller("a.ts", "comp", &["cn"], &[], &[]),
            sym("b.ts", "cn"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.ts:comp").unwrap());
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].name, "cn");
    }

    #[test]
    fn non_callable_targets_receive_no_edges() {
        let g = SymbolGraph::new(vec![
            caller("a.ts", "schema", &["primaryKey"], &[], &[]),
            variable("b.ts", "primaryKey"),
        ]);
        assert!(g
            .callers(&Coordinate::parse("b.ts:primaryKey").unwrap())
            .is_empty());
        assert_eq!(g.resolve_stats.resolved, 0);
        assert_eq!(g.resolve_stats.unresolved, 1);
    }

    #[test]
    fn callable_kind_gate_covers_every_kind() {
        use SymbolKind::*;
        for kind in [Function, Method, Class, Struct, Enum] {
            assert!(is_callable_kind(&kind));
        }
        for kind in [
            Variable, Constant, Field, Interface, Trait, TypeAlias, Module,
        ] {
            assert!(!is_callable_kind(&kind));
        }
    }

    #[test]
    fn external_imports_never_resolve_locally_including_qualified_prefixes() {
        let g = SymbolGraph::new(vec![
            caller(
                "a.go",
                "main",
                &["os.Open", "primaryKey"],
                &[],
                &["os", "primaryKey"],
            ),
            sym("b.go", "Open"),
            variable("b.ts", "primaryKey"),
        ]);
        assert!(g
            .callers(&Coordinate::parse("b.go:Open").unwrap())
            .is_empty());
        assert_eq!(g.resolve_stats.external, 2);
        assert_eq!(g.resolve_stats.resolved, 0);
    }

    #[test]
    fn test_callers_quarantined_in_pagerank() {
        let g = SymbolGraph::new(vec![
            caller("tests/t.rs", "t", &["test_only_hub"], &[], &[]),
            caller("src/p.rs", "p", &["prod_hub"], &[], &[]),
            sym("src/hub1.rs", "test_only_hub"),
            sym("src/hub2.rs", "prod_hub"),
        ]);
        let rank_of = |name: &str| {
            g.symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .centrality
        };
        assert!(rank_of("prod_hub") > rank_of("test_only_hub"));
        assert!(rank_of("p") > rank_of("t"));
    }
}
