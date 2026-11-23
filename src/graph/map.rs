use crate::graph::blast::{is_program_entry, is_test_symbol};
use crate::graph::SymbolGraph;
use crate::model::{Coordinate, Symbol, SymbolKind};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Default symbol cap when `--limit` is omitted (`0` means unlimited).
pub const DEFAULT_MAP_LIMIT: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapSymbol {
    pub name: String,
    pub kind: String,
    pub coordinate: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub centrality: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMap {
    pub file: String,
    pub symbols: Vec<MapSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapResult {
    pub scope: Option<String>,
    pub focus: Option<String>,
    pub modules: Vec<ModuleMap>,
    pub total_symbols: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_from: Option<usize>,
    #[serde(default)]
    pub resolved: usize,
    #[serde(default)]
    pub ambiguous_dropped: usize,
    #[serde(default)]
    pub unresolved: usize,
    #[serde(default)]
    pub external: usize,
    #[serde(default)]
    pub pagerank_iters: usize,
    #[serde(default)]
    pub pagerank_converged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crawled_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unindexed_exts: Option<Vec<String>>,
}

impl MapResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Repository Map (Ranked by Centrality)\n\n");

        if let Some(scope) = &self.scope {
            out.push_str(&format!("*Scope*: `{}` | ", scope));
        }
        if let Some(focus) = &self.focus {
            out.push_str(&format!("*Focus*: `{}` | ", focus));
        }
        out.push_str(&format!("*Total Symbols*: {}", self.total_symbols));
        if let Some(total) = self.truncated_from {
            out.push_str(&format!(" of {} (--limit; pass --limit 0 for all)", total));
        }
        out.push_str(&format!(
            "\n*Edges*: {} resolved, {} ambiguous-dropped, {} unresolved, {} external | *PageRank*: {} iters, {}",
            self.resolved,
            self.ambiguous_dropped,
            self.unresolved,
            self.external,
            self.pagerank_iters,
            if self.pagerank_converged {
                "converged"
            } else {
                "NOT converged"
            }
        ));
        if let (Some(idx), Some(crawled)) = (self.indexed_files, self.crawled_files) {
            let pct = idx * 100 / crawled.max(1);
            out.push_str(&format!(
                "\n*Coverage*: {idx}/{crawled} files indexed ({pct}%)"
            ));
            if let Some(exts) = &self.unindexed_exts {
                if !exts.is_empty() {
                    out.push_str(&format!(" | skipped exts: {}", exts.join(", ")));
                }
            }
        }
        out.push_str("\n\n");

        if self.modules.is_empty() {
            out.push_str("No symbols matching the specified scope.\n");
            return out;
        }

        for module in &self.modules {
            out.push_str(&format!("### `{}`\n", module.file));
            for s in &module.symbols {
                out.push_str(&format!(
                    "- **`{}`** ({}) [rank: {:.4}] -> L{}-L{}\n",
                    s.name, s.kind, s.centrality, s.start_line, s.end_line
                ));
                if !s.signature.is_empty() {
                    out.push_str(&format!("    `{}`\n", s.signature));
                }
            }
            out.push('\n');
        }

        out
    }

    /// Compact M2M map format shared with Turn-0 `dump`.
    pub fn format_compact_markdown(&self) -> String {
        let mut out = String::new();
        if let (Some(idx), Some(crawled)) = (self.indexed_files, self.crawled_files) {
            let pct = idx * 100 / crawled.max(1);
            out.push_str(&format!(
                "_Coverage: {idx}/{crawled} files ({pct}%) | Edges: {} resolved, {} external, {} unresolved_\n\n",
                self.resolved, self.external, self.unresolved
            ));
        }
        if self.modules.is_empty() {
            out.push_str("No indexed symbols found in workspace.\n\n");
            return out;
        }
        for module in &self.modules {
            out.push_str(&format!("### `{}`\n", module.file));
            for s in &module.symbols {
                out.push_str(&format!(
                    "- **`{}`** ({}) [rank: {:.4}] -> L{}-L{}\n",
                    s.name, s.kind, s.centrality, s.start_line, s.end_line
                ));
                if !s.signature.is_empty() {
                    out.push_str(&format!("    `{}`\n", s.signature));
                }
            }
            out.push('\n');
        }
        out
    }
}

pub fn generate_map(
    graph: &SymbolGraph,
    scope: Option<&str>,
    focus: Option<&str>,
    limit: Option<usize>,
) -> MapResult {
    generate_map_with_scores(graph, None, scope, focus, limit)
}

fn scope_targets_tests(scope: Option<&str>) -> bool {
    let Some(sc) = scope else {
        return false;
    };
    sc.split('/').any(|seg| {
        matches!(seg, "tests" | "test" | "__tests__" | "spec")
            || seg.ends_with("_test.go")
            || seg.ends_with("_test.py")
            || seg.contains(".test.")
            || seg.contains(".spec.")
    })
}

/// Composite orientation rank used when unseeded/unfocused: elevates program
/// entry points and public API surfaces while demoting private 1–5 LOC leaf helpers.
fn orientation_score(graph: &SymbolGraph, idx: usize, sym: &Symbol, pr_score: f64) -> f64 {
    let mut weight = 1.0;
    if is_program_entry(sym) {
        weight *= 2.5;
    } else if sym.is_public() {
        weight *= 1.35;
    }
    if matches!(
        sym.kind,
        SymbolKind::Struct | SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
    ) {
        weight *= 1.15;
    }
    let span = sym.end_line.saturating_sub(sym.start_line) + 1;
    let out_deg = graph.callees_map.get(&idx).map_or(0, Vec::len);
    if !sym.is_public() && !is_program_entry(sym) && span <= 5 && out_deg == 0 {
        weight *= 0.25;
    }
    pr_score * weight
}

/// Coarse module bucket (first 2 path segments for nested files, or file path for shallow files)
/// used for round-robin stratified coverage across subsystems.
fn module_bucket(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() >= 3 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        file.to_string()
    }
}

type RankedCandidate<'a> = (usize, &'a Symbol, f64, f64);

fn stratify_selection<'a>(
    candidates: Vec<RankedCandidate<'a>>,
    limit: usize,
) -> Vec<(usize, &'a Symbol, f64)> {
    let mut buckets: BTreeMap<String, Vec<RankedCandidate<'a>>> = BTreeMap::new();
    for item in candidates {
        buckets
            .entry(module_bucket(&item.1.file))
            .or_default()
            .push(item);
    }

    let mut ranked_buckets: Vec<(f64, String, Vec<RankedCandidate<'a>>)> = buckets
        .into_iter()
        .map(|(k, mut list)| {
            list.sort_by(|(_, a, _, ra), (_, b, _, rb)| {
                rb.total_cmp(ra)
                    .then_with(|| a.file.cmp(&b.file))
                    .then_with(|| a.start_line.cmp(&b.start_line))
                    .then_with(|| a.name.cmp(&b.name))
            });
            let peak = list.first().map(|x| x.3).unwrap_or(0.0);
            (peak, k, list)
        })
        .collect();
    ranked_buckets.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut picked: Vec<(usize, &'a Symbol, f64)> = Vec::with_capacity(limit);
    let mut depth = 0usize;
    while picked.len() < limit {
        let mut progressed = false;
        for (_, _, list) in &ranked_buckets {
            if let Some(&(idx, sym, pr, _)) = list.get(depth) {
                picked.push((idx, sym, pr));
                progressed = true;
                if picked.len() >= limit {
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
        depth += 1;
    }
    picked
}

pub fn generate_map_with_scores(
    graph: &SymbolGraph,
    personalized: Option<(&[f64], &crate::graph::pagerank::PagerankStats)>,
    scope: Option<&str>,
    focus: Option<&str>,
    limit: Option<usize>,
) -> MapResult {
    let clean_scope = scope
        .map(|s| s.trim_start_matches("./").trim_end_matches('/'))
        .filter(|s| !s.is_empty() && *s != ".");

    let score_of = |idx: usize| -> f64 {
        personalized
            .and_then(|(scores, _)| scores.get(idx).copied())
            .unwrap_or(graph.symbols[idx].centrality)
    };

    let scoped: Vec<(usize, &Symbol, f64)> = graph
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| match clean_scope {
            None => true,
            Some(sc) => in_scope(&s.file, sc),
        })
        .map(|(idx, s)| (idx, s, score_of(idx)))
        .collect();

    let exclude_tests = personalized.is_none() && !scope_targets_tests(clean_scope);
    let filtered: Vec<(usize, &Symbol, f64)> = if exclude_tests {
        let prod: Vec<_> = scoped
            .iter()
            .copied()
            .filter(|(_, s, _)| !is_test_symbol(s))
            .collect();
        if prod.is_empty() {
            scoped
        } else {
            prod
        }
    } else {
        scoped
    };

    let total_symbols = filtered.len();
    let effective_limit = match limit {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(DEFAULT_MAP_LIMIT),
    };

    let mut ranked: Vec<(usize, &Symbol, f64, f64)> = filtered
        .into_iter()
        .map(|(idx, s, pr)| {
            let r = if personalized.is_some() {
                pr
            } else {
                orientation_score(graph, idx, s, pr)
            };
            (idx, s, pr, r)
        })
        .collect();

    ranked.sort_by(|(_, a, _, ra), (_, b, _, rb)| {
        rb.total_cmp(ra)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });

    let (selected, truncated_from): (Vec<(usize, &Symbol, f64)>, Option<usize>) =
        match effective_limit {
            Some(n) if n < total_symbols => {
                let picked = if personalized.is_none() {
                    stratify_selection(ranked, n)
                } else {
                    ranked
                        .into_iter()
                        .take(n)
                        .map(|(i, s, pr, _)| (i, s, pr))
                        .collect()
                };
                (picked, Some(total_symbols))
            }
            _ => (
                ranked.into_iter().map(|(i, s, pr, _)| (i, s, pr)).collect(),
                None,
            ),
        };

    let mut file_groups: BTreeMap<String, Vec<MapSymbol>> = BTreeMap::new();
    for &(_, s, centrality) in &selected {
        file_groups
            .entry(s.file.clone())
            .or_default()
            .push(MapSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                coordinate: s.coordinate(),
                start_line: s.start_line,
                end_line: s.end_line,
                signature: s.signature.clone(),
                centrality,
            });
    }

    let mut modules: Vec<ModuleMap> = file_groups
        .into_iter()
        .map(|(file, mut symbols)| {
            symbols.sort_by(|a, b| {
                b.centrality
                    .total_cmp(&a.centrality)
                    .then_with(|| a.start_line.cmp(&b.start_line))
                    .then_with(|| a.name.cmp(&b.name))
            });
            ModuleMap { file, symbols }
        })
        .collect();

    modules.sort_by(|a, b| {
        let max_a = a.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        let max_b = b.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        max_b.total_cmp(&max_a).then_with(|| a.file.cmp(&b.file))
    });

    let (pagerank_iters, pagerank_converged) = match personalized {
        Some((_, stats)) => (stats.iters, stats.converged),
        None => (graph.pagerank_iters, graph.pagerank_converged),
    };

    MapResult {
        scope: scope.map(|s| s.to_string()),
        focus: focus.map(|s| s.to_string()),
        modules,
        total_symbols: selected.len(),
        truncated_from,
        resolved: graph.resolve_stats.resolved,
        ambiguous_dropped: graph.resolve_stats.ambiguous_dropped,
        unresolved: graph.resolve_stats.unresolved,
        external: graph.resolve_stats.external,
        pagerank_iters,
        pagerank_converged,
        indexed_files: graph.coverage.as_ref().map(|c| c.indexed_files),
        crawled_files: graph.coverage.as_ref().map(|c| c.crawled_files),
        unindexed_exts: graph.coverage.as_ref().map(|c| c.unindexed_exts.clone()),
    }
}

/// Strict path-component prefix match (no mid-path or basename over-matching).
fn in_scope(file: &str, scope: &str) -> bool {
    let scope = scope
        .trim_start_matches("./")
        .trim_end_matches('/')
        .replace('\\', "/");
    if scope.is_empty() || scope == "." {
        return true;
    }
    let file = file.trim_start_matches("./");
    file == scope
        || (file.len() > scope.len()
            && file.starts_with(&scope)
            && file.as_bytes()[scope.len()] == b'/')
}

/// Validate a map `--scope` parameter against workspace root before filtering.
pub fn validate_map_scope(root: &Path, scope: Option<&str>) -> Result<Option<String>> {
    match scope {
        None => Ok(None),
        Some(raw) => {
            let parsed = crate::workspace::walker::WorkspaceScope::parse(root, raw, false)?;
            Ok((!parsed.rel_prefix.is_empty()).then_some(parsed.rel_prefix))
        }
    }
}

/// Compute personalized PageRank scores without cloning `SymbolGraph`.
/// Fails closed if `focus` is missing or ambiguous.
pub fn compute_personalized_scores(
    graph: &SymbolGraph,
    focus: Option<&str>,
    seed: Option<&str>,
    cwd: &Path,
) -> Result<Option<(Vec<f64>, crate::graph::pagerank::PagerankStats)>> {
    let mut indices = Vec::new();

    if let Some(target) = focus {
        let coord = Coordinate::parse(target)?.normalize_against(cwd);
        let idx = graph.resolve_strict(&coord, "map --focus")?;
        indices.push(idx);
    }
    if let Some(term) = seed {
        let seeded = graph.seed_indices(term);
        if seeded.is_empty() {
            eprintln!(
                "Warning: --seed '{}' matched 0 symbols; using default ranking.",
                term
            );
        }
        indices.extend(seeded);
    }

    indices.sort_unstable();
    indices.dedup();

    if indices.is_empty() {
        return Ok(None);
    }

    Ok(Some(crate::graph::pagerank::compute_pagerank_scores(
        &graph.symbols,
        &graph.callers_map,
        &graph.callees_map,
        &graph.edge_weights,
        Some(&indices),
    )))
}

pub fn personalize_map(
    graph: &mut SymbolGraph,
    focus: Option<&str>,
    seed: Option<&str>,
    cwd: &Path,
) -> Result<()> {
    if let Some((scores, stats)) = compute_personalized_scores(graph, focus, seed, cwd)? {
        for (sym, score) in graph.symbols.iter_mut().zip(scores) {
            sym.centrality = score;
        }
        graph.pagerank_iters = stats.iters;
        graph.pagerank_converged = stats.converged;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Symbol, SymbolKind};

    fn make_sym(file: &str, name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 10,
            signature: format!("pub fn {name}()"),
            body: String::new(),
            centrality: 0.0,
            calls: vec![],
            mentions: vec![],
            call_counts: Default::default(),
            member_calls: vec![],
            external_imports: crate::model::symbol::empty_external_imports(),
            flags: crate::model::symbol_flags::IS_PUBLIC,
        }
    }

    #[test]
    fn map_header_reports_resolution_convergence_and_coverage() {
        let mut g = SymbolGraph::new(vec![make_sym("src/a.rs", "a")]);
        g.coverage = Some(crate::graph::CoverageStats {
            indexed_files: 1,
            crawled_files: 4,
            unindexed_exts: vec!["md×3".to_string()],
        });
        let map = generate_map(&g, None, None, None);
        let md = map.to_markdown();
        assert!(md.contains("ambiguous-dropped"), "got: {md}");
        assert!(md.contains("PageRank"), "got: {md}");
        assert!(md.contains("1/4 files indexed"), "got: {md}");
        assert!(md.contains("md×3"), "got: {md}");
    }

    #[test]
    fn map_defaults_to_30_and_stratifies_across_modules() {
        let mut syms = Vec::new();
        for i in 0..40 {
            syms.push(make_sym("src/heavy/mod.rs", &format!("heavy_{i}")));
        }
        syms.push(make_sym("src/other/entry.rs", "other_entry"));
        let g = SymbolGraph::new(syms);
        let map = generate_map(&g, None, None, None);
        assert_eq!(map.total_symbols, DEFAULT_MAP_LIMIT);
        assert_eq!(map.truncated_from, Some(41));
        assert!(
            map.modules.iter().any(|m| m.file == "src/other/entry.rs"),
            "stratification must keep src/other/entry.rs visible"
        );
        let all = generate_map(&g, None, None, Some(0));
        assert_eq!(all.total_symbols, 41);
        assert_eq!(all.truncated_from, None);
    }

    #[test]
    fn in_scope_does_not_overmatch_basenames_in_other_dirs() {
        assert!(in_scope("src/graph/mod.rs", "src/graph"));
        assert!(!in_scope("src/graph/mod.rs", "mod.rs"));
        assert!(!in_scope("src/graph_extra/mod.rs", "src/graph"));
    }
}
