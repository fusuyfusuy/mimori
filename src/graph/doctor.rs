use crate::graph::blast::{is_program_entry, is_test_symbol};
use crate::graph::SymbolGraph;
use crate::model::{Symbol, SymbolKind};
use crate::workspace::walker::WorkspaceScope;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_DOCTOR_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub centrality: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubSymbol {
    pub name: String,
    pub kind: String,
    pub coordinate: String,
    pub fan_in: usize,
    pub centrality: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorResult {
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    pub isolated: usize,
    pub likely_dead: Vec<DeadSymbol>,
    pub needs_review: Vec<DeadSymbol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub likely_dead_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_review_total: Option<usize>,
    pub top_hubs: Vec<HubSymbol>,
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
}

impl DoctorResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Doctor: repository health\n\n");
        out.push_str(&format!(
            "- Files: {}\n- Symbols: {}\n- Edges: {}\n- Isolated (no callers, no callees): {}\n- Resolution: {} resolved, {} ambiguous-dropped, {} unresolved, {} external\n- PageRank: {} iters, {}\n\n",
            self.files,
            self.symbols,
            self.edges,
            self.isolated,
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

        if self.top_hubs.is_empty() {
            out.push_str("No hubs found.\n");
        } else {
            out.push_str("## Top hubs by fan-in\n");
            for h in &self.top_hubs {
                out.push_str(&format!(
                    "- **`{}`** ({}) fan-in {} rank {:.4} -> `{}`\n",
                    h.name, h.kind, h.fan_in, h.centrality, h.coordinate
                ));
            }
            out.push('\n');
        }

        let total_likely = self.likely_dead_total.unwrap_or(self.likely_dead.len());
        let total_review = self.needs_review_total.unwrap_or(self.needs_review.len());

        if total_likely == 0 && total_review == 0 {
            out.push_str("No dead-weight candidates. Nothing is both uncalled and unconnected.\n");
        } else {
            out.push_str(&format!(
                "## Dead-weight candidates ({} likely, {} to review)\n",
                total_likely, total_review
            ));
            if self.likely_dead.len() < total_likely || self.needs_review.len() < total_review {
                out.push_str(&format!(
                    "_Showing {} of {} likely, {} of {} to review (--limit)._\n",
                    self.likely_dead.len(),
                    total_likely,
                    self.needs_review.len(),
                    total_review
                ));
            }
            out.push_str(
                "_Uncalled, unconnected, not `main`, not a test, not a trait impl or attribute hook._\n",
            );
            if !self.likely_dead.is_empty() {
                out.push_str("\n### Likely dead (isolated functions)\n");
                for d in &self.likely_dead {
                    out.push_str(&format!(
                        "- **`{}`** ({}) rank {:.4} -> `{}`\n",
                        d.name, d.kind, d.centrality, d.coordinate
                    ));
                }
            }
            if !self.needs_review.is_empty() {
                out.push_str("\n### Needs human review (isolated types/constants)\n");
                for d in &self.needs_review {
                    out.push_str(&format!(
                        "- **`{}`** ({}) rank {:.4} -> `{}`\n",
                        d.name, d.kind, d.centrality, d.coordinate
                    ));
                }
            }
        }

        out
    }
}

/// Framework-dispatched entries reachable without a tracked call edge.
pub(crate) fn is_framework_entry(sym: &Symbol) -> bool {
    let file = sym.file.replace('\\', "/");
    let filename = file.rsplit('/').next().unwrap_or(&file);
    let stem = filename.split('.').next().unwrap_or("");
    if matches!(
        stem,
        "page" | "route" | "layout" | "loading" | "error" | "middleware" | "server"
    ) {
        return true;
    }
    let leaf = sym.name.rsplit("::").next().unwrap_or(&sym.name);
    let leaf = leaf.rsplit('.').next().unwrap_or(leaf);
    if leaf == "appRouter" || leaf == "router" || leaf.ends_with("Router") {
        return true;
    }
    if sym.name.starts_with("appRouter::") || sym.name.starts_with("appRouter.") {
        return true;
    }
    if stem == "index"
        || (stem == "mod"
            && (sym.kind == SymbolKind::TypeAlias || sym.signature.starts_with("pub ")))
    {
        return true;
    }
    false
}

fn in_doctor_scope(file: &str, scope: Option<&WorkspaceScope>) -> bool {
    scope.is_none_or(|sc| sc.matches_rel(file))
}

pub fn run_doctor(graph: &SymbolGraph, limit: Option<usize>) -> DoctorResult {
    run_doctor_scoped(graph, limit, None)
}

pub fn run_doctor_scoped(
    graph: &SymbolGraph,
    limit: Option<usize>,
    scope: Option<&WorkspaceScope>,
) -> DoctorResult {
    let scoped_indices: Vec<usize> = graph
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| in_doctor_scope(&s.file, scope))
        .map(|(i, _)| i)
        .collect();

    let files: HashSet<&str> = scoped_indices
        .iter()
        .map(|&i| graph.symbols[i].file.as_str())
        .collect();
    let edges: usize = scoped_indices
        .iter()
        .map(|&i| graph.callees_map.get(&i).map_or(0, Vec::len))
        .sum();
    let mentioned_names: HashSet<&str> = graph
        .mention_to_indices
        .keys()
        .map(String::as_str)
        .collect();

    let mut isolated = 0usize;
    let mut likely_dead = Vec::new();
    let mut needs_review = Vec::new();
    for &idx in &scoped_indices {
        let sym = &graph.symbols[idx];
        let has_callers = graph.callers_map.get(&idx).is_some_and(|v| !v.is_empty());
        let has_callees = graph.callees_map.get(&idx).is_some_and(|v| !v.is_empty());
        if !has_callers && !has_callees {
            isolated += 1;
        }
        let leaf = sym.name.rsplit([':', '.']).next().unwrap_or(&sym.name);
        let is_mentioned =
            mentioned_names.contains(sym.name.as_str()) || mentioned_names.contains(leaf);
        let has_unresolved_member =
            sym.kind == SymbolKind::Method && graph.unresolved_member_calls.contains_key(leaf);

        let is_unreferenced = (!has_callers && !has_callees)
            || (!has_callers
                && !sym.is_public()
                && matches!(sym.kind, SymbolKind::Function | SymbolKind::Method));

        if is_unreferenced
            && !is_mentioned
            && !has_unresolved_member
            && !is_program_entry(sym)
            && !is_test_symbol(sym)
            && !sym.is_trait_impl()
            && !sym.is_attr_entry()
            && !is_framework_entry(sym)
        {
            let dead = DeadSymbol {
                name: sym.name.clone(),
                kind: sym.kind.as_str().to_string(),
                file: sym.file.clone(),
                coordinate: sym.coordinate(),
                centrality: sym.centrality,
            };
            match sym.kind {
                SymbolKind::Function => likely_dead.push(dead),
                _ => needs_review.push(dead),
            }
        }
    }

    let by_centrality = |a: &DeadSymbol, b: &DeadSymbol| {
        a.centrality
            .total_cmp(&b.centrality)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.coordinate.cmp(&b.coordinate))
    };
    likely_dead.sort_by(by_centrality);
    needs_review.sort_by(by_centrality);

    let raw_likely_total = likely_dead.len();
    let raw_review_total = needs_review.len();
    let cap = limit.unwrap_or(50).min(MAX_DOCTOR_LIMIT);
    likely_dead.truncate(cap);
    needs_review.truncate(cap);

    let likely_dead_total = (likely_dead.len() < raw_likely_total).then_some(raw_likely_total);
    let needs_review_total = (needs_review.len() < raw_review_total).then_some(raw_review_total);

    let mut hubs: Vec<HubSymbol> = scoped_indices
        .iter()
        .copied()
        .map(|idx| (idx, &graph.symbols[idx]))
        .filter(|(_, s)| !is_test_symbol(s))
        .filter_map(|(idx, s)| {
            let fan_in = graph.callers_map.get(&idx).map_or(0, |v| {
                v.iter()
                    .filter(|&&caller_idx| !is_test_symbol(&graph.symbols[caller_idx]))
                    .count()
            });
            (fan_in > 0).then(|| HubSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                coordinate: s.coordinate(),
                fan_in,
                centrality: s.centrality,
            })
        })
        .collect();
    hubs.sort_by(|a, b| {
        b.fan_in
            .cmp(&a.fan_in)
            .then_with(|| b.centrality.total_cmp(&a.centrality))
            .then_with(|| a.coordinate.cmp(&b.coordinate))
    });
    hubs.truncate(5);

    DoctorResult {
        files: files.len(),
        symbols: scoped_indices.len(),
        edges,
        isolated,
        likely_dead,
        needs_review,
        likely_dead_total,
        needs_review_total,
        top_hubs: hubs,
        resolved: graph.resolve_stats.resolved,
        ambiguous_dropped: graph.resolve_stats.ambiguous_dropped,
        unresolved: graph.resolve_stats.unresolved,
        external: graph.resolve_stats.external,
        pagerank_iters: graph.pagerank_iters,
        pagerank_converged: graph.pagerank_converged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::SymbolGraph;
    use crate::model::{symbol_flags, Symbol, SymbolKind};
    use std::collections::BTreeMap;

    fn sym(file: &str, name: &str, refs: Vec<String>) -> Symbol {
        let mut counts = BTreeMap::new();
        for r in &refs {
            counts.insert(r.clone(), 1);
        }
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 5,
            signature: String::new(),
            body: format!("fn {name} {{}}"),
            centrality: 0.0,
            calls: refs,
            mentions: vec![],
            call_counts: counts,
            member_calls: vec![],
            external_imports: crate::model::symbol::empty_external_imports(),
            flags: 0,
        }
    }

    #[test]
    fn isolated_non_entry_is_flagged_dead() {
        let g = SymbolGraph::new(vec![
            sym("src/a.rs", "used", vec![]),
            sym("src/a.rs", "caller", vec!["used".into()]),
            sym("src/b.rs", "lonely", vec![]),
        ]);
        let res = run_doctor(&g, None);
        assert_eq!(res.symbols, 3);
        assert!(res.likely_dead.iter().any(|d| d.name == "lonely"));
        assert!(!res.likely_dead.iter().any(|d| d.name == "used"));
        assert!(!res.needs_review.iter().any(|d| d.name == "used"));
    }

    #[test]
    fn isolated_constants_need_review_not_likely_dead() {
        let mut c = sym("src/b.rs", "STRAY_CONST", vec![]);
        c.kind = SymbolKind::Constant;
        let g = SymbolGraph::new(vec![c]);
        let res = run_doctor(&g, None);
        assert!(res.likely_dead.is_empty());
        assert!(res.needs_review.iter().any(|d| d.name == "STRAY_CONST"));
    }

    #[test]
    fn entry_points_tests_trait_impls_and_attr_hooks_are_excluded() {
        let mut trait_fn = sym("src/lib.rs", "Foo::default", vec![]);
        trait_fn.flags = symbol_flags::IS_TRAIT_IMPL;
        let mut serde_fn = sym("src/lib.rs", "default_direction", vec![]);
        serde_fn.flags = symbol_flags::IS_ATTR_ENTRY;
        let mut cfg_test_fn = sym("src/lib.rs", "fixture", vec![]);
        cfg_test_fn.flags = symbol_flags::IS_TEST;

        let g = SymbolGraph::new(vec![
            sym("src/main.rs", "main", vec![]),
            sym("tests/cli.rs", "helper", vec![]),
            trait_fn,
            serde_fn,
            cfg_test_fn,
        ]);
        let res = run_doctor(&g, None);
        assert!(res.likely_dead.is_empty() && res.needs_review.is_empty());
    }

    #[test]
    fn framework_dispatched_entries_are_excluded() {
        let mut router_proc = sym("src/routers/user.ts", "appRouter::getUser", vec![]);
        router_proc.kind = SymbolKind::Method;
        let g = SymbolGraph::new(vec![
            router_proc,
            sym("src/app/page.tsx", "Page", vec![]),
            sym("src/app/api/route.ts", "GET", vec![]),
            sym("src/middleware.ts", "middleware", vec![]),
            sym("src/index.ts", "publicHelper", vec![]),
        ]);
        let res = run_doctor(&g, None);
        assert!(res.likely_dead.is_empty() && res.needs_review.is_empty());
    }

    #[test]
    fn likely_dead_caps_at_fifty_and_limit_zero_reports_totals() {
        let syms: Vec<Symbol> = (0..60)
            .map(|i| sym(&format!("src/f{i}.rs"), &format!("lonely{i}"), vec![]))
            .collect();
        let g = SymbolGraph::new(syms);
        let res = run_doctor(&g, None);
        assert_eq!(res.likely_dead.len(), 50);
        assert_eq!(res.likely_dead_total, Some(60));

        let res_zero = run_doctor(&g, Some(0));
        let md = res_zero.to_markdown();
        assert!(md.contains("60 likely"), "got: {md}");
        assert!(!md.contains("No dead-weight candidates"), "got: {md}");
    }

    #[test]
    fn doctor_reports_resolution_and_convergence() {
        let g = SymbolGraph::new(vec![
            sym("src/a.rs", "caller", vec!["used".into()]),
            sym("src/a.rs", "used", vec![]),
        ]);
        let res = run_doctor(&g, None);
        assert_eq!(res.resolved, 1);
        assert!(res.pagerank_iters >= 1);
        let md = res.to_markdown();
        assert!(md.contains("ambiguous-dropped"));
        assert!(md.contains("PageRank"));
    }

    #[test]
    fn uncalled_private_function_with_callees_is_flagged_likely_dead() {
        let mut pub_entry = sym("src/a.rs", "pub_entry", vec!["helper".into()]);
        pub_entry.flags = symbol_flags::IS_PUBLIC;
        let dead_private = sym("src/a.rs", "dead_private", vec!["helper".into()]);
        let helper = sym("src/a.rs", "helper", vec![]);

        let g = SymbolGraph::new(vec![pub_entry, dead_private, helper]);
        let res = run_doctor(&g, None);
        assert!(res.likely_dead.iter().any(|d| d.name == "dead_private"));
        assert!(!res.likely_dead.iter().any(|d| d.name == "pub_entry"));
        assert!(!res.likely_dead.iter().any(|d| d.name == "helper"));
    }

    #[test]
    fn top_hubs_excludes_test_callers_from_fan_in() {
        let mut t1 = sym("tests/t.rs", "t1", vec!["hub".into()]);
        t1.flags = symbol_flags::IS_TEST;
        let mut t2 = sym("tests/t.rs", "t2", vec!["hub".into()]);
        t2.flags = symbol_flags::IS_TEST;
        let prod = sym("src/app.rs", "run", vec!["hub".into()]);
        let hub = sym("src/core.rs", "hub", vec![]);

        let g = SymbolGraph::new(vec![t1, t2, prod, hub]);
        let res = run_doctor(&g, None);
        let hub_entry = res.top_hubs.iter().find(|h| h.name == "hub").unwrap();
        assert_eq!(hub_entry.fan_in, 1);
    }

    #[test]
    fn run_doctor_scoped_filters_to_scope_while_preserving_cross_package_callers() {
        let mut exported = sym("packages/app/api.rs", "used_from_web", vec![]);
        exported.flags = symbol_flags::IS_PUBLIC;
        let dead_in_app = sym("packages/app/dead.rs", "dead_in_app", vec![]);
        let caller_in_web = sym(
            "packages/web/page.rs",
            "render",
            vec!["used_from_web".into()],
        );
        let dead_in_web = sym("packages/web/dead.rs", "dead_in_web", vec![]);

        let g = SymbolGraph::new(vec![exported, dead_in_app, caller_in_web, dead_in_web]);
        let dir = tempfile::tempdir().unwrap();
        let scope = WorkspaceScope::parse(dir.path(), "packages/app", false).unwrap();
        let res = run_doctor_scoped(&g, None, Some(&scope));
        assert_eq!(res.files, 2);
        assert_eq!(res.symbols, 2);
        assert!(res.likely_dead.iter().any(|d| d.name == "dead_in_app"));
        assert!(!res.likely_dead.iter().any(|d| d.name == "dead_in_web"));
        assert!(!res.likely_dead.iter().any(|d| d.name == "used_from_web"));
    }
}
