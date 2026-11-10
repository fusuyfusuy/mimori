use crate::graph::SymbolGraph;
use crate::model::Coordinate;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A symbol as it appears in a map.
///
/// Deliberately not `Symbol`: that type carries `body`, so `map --json` used to
/// serialize the entire source of the repository -- 2.0x the bytes it indexed.
/// Source is what `slice` is for.
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
    /// Set when `--limit` dropped lower-ranked symbols from the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_from: Option<usize>,
    /// Edge-resolution accounting (P0-2): what the resolver kept vs dropped.
    #[serde(default)]
    pub resolved: usize,
    #[serde(default)]
    pub ambiguous_dropped: usize,
    #[serde(default)]
    pub unresolved: usize,
    /// Calls skipped as external imports (never resolved locally).
    #[serde(default)]
    pub external: usize,
    /// PageRank convergence (P1-1): iterations run + whether L1 hit 1e-6.
    #[serde(default)]
    pub pagerank_iters: usize,
    #[serde(default)]
    pub pagerank_converged: bool,
    /// Coverage honesty (P1-4): indexed vs crawled files + skipped extensions.
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
            out.push_str(&format!(" of {} (--limit)", total));
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
            out.push_str(&format!("### 📁 `{}`\n", module.file));
            for s in &module.symbols {
                out.push_str(&format!(
                    "- 🔹 **`{}`** ({}) [rank: {:.4}] → L{}-L{}\n",
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

    let mut selected: Vec<(usize, &crate::model::Symbol, f64)> = graph
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| match clean_scope {
            None => true,
            Some(sc) => in_scope(&s.file, sc),
        })
        .map(|(idx, s)| (idx, s, score_of(idx)))
        .collect();

    selected.sort_by(|(_, a, sa), (_, b, sb)| {
        sb.total_cmp(sa)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });

    let total_symbols = selected.len();
    let truncated_from = match limit {
        Some(n) if n < total_symbols => {
            selected.truncate(n);
            Some(total_symbols)
        }
        _ => None,
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

    // Rank modules by their strongest symbol with deterministic file tie-break.
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

/// Scope matches on path components, so `--scope src` does not also select
/// `foo/src2/`.
fn in_scope(file: &str, scope: &str) -> bool {
    let scope = scope.trim_start_matches("./").trim_end_matches('/');
    if scope.is_empty() || scope == "." {
        return true;
    }
    let file = file.trim_start_matches("./");
    file == scope
        || (file.starts_with(scope) && file.as_bytes().get(scope.len()) == Some(&b'/'))
        || file.split('/').any(|c| c == scope)
}

/// Compute personalized PageRank scores without cloning or mutating `SymbolGraph`.
/// Fails closed if `focus` is specified and resolves to zero symbols.
pub fn compute_personalized_scores(
    graph: &SymbolGraph,
    focus: Option<&str>,
    seed: Option<&str>,
    cwd: &Path,
) -> Result<Option<(Vec<f64>, crate::graph::pagerank::PagerankStats)>> {
    let mut indices = Vec::new();

    if let Some(target) = focus {
        let coord = Coordinate::parse(target)?.normalize_against(cwd);
        let resolved = graph.resolve_all(&coord);
        if resolved.is_empty() {
            anyhow::bail!("{}", graph.format_not_found_error(&coord));
        }
        indices.extend(resolved);
    }
    if let Some(term) = seed {
        indices.extend(graph.seed_indices(term));
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

/// Apply `--focus` (personalized PageRank around a symbol) and/or `--seed`
/// (bias toward symbols matching a term) to the ranking.
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

    #[test]
    fn map_header_reports_resolution_convergence_and_coverage() {
        // P0-2/P1-1/P1-4: the header discloses what the resolver dropped,
        // the pagerank exit, and the indexed-file ratio.
        let mut g = SymbolGraph::new(vec![Symbol {
            name: "a".into(),
            kind: SymbolKind::Function,
            file: "src/a.rs".into(),
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
        }]);
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
}
