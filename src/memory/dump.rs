use crate::graph::map::{compute_personalized_scores, generate_map_with_scores, MapResult};
use crate::graph::SymbolGraph;
use crate::memory::ledger::{confine_agents_path, index_adr_titles, MemoryLedger};
use crate::storage::get_or_sync_graph;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpResult {
    pub budget: usize,
    pub estimated_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epics: Option<String>,
    pub vocab_gotchas: Option<String>,
    pub active_debt: Vec<String>,
    pub debt_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adrs: Vec<String>,
    pub map: Option<MapResult>,
    pub markdown: String,
}

pub fn generate_dump(
    workspace_root: &Path,
    budget: usize,
    focus: Option<&str>,
) -> Result<DumpResult> {
    let graph = get_or_sync_graph(workspace_root)?;
    generate_dump_with_graph(workspace_root, Some(&graph), budget, focus)
}

pub fn generate_dump_with_graph(
    workspace_root: &Path,
    graph: Option<&SymbolGraph>,
    budget: usize,
    focus: Option<&str>,
) -> Result<DumpResult> {
    let char_budget = budget.saturating_mul(4);

    // 1. Read memory ledger if present (fail closed on path escape, degrade on malformed UTF-8).
    let mem_path = confine_agents_path(workspace_root, ".agents/memory.md")?;
    let mut memory_warning: Option<String> = None;
    let ledger = if mem_path.exists() {
        match MemoryLedger::load(workspace_root) {
            Ok(l) => Some(l),
            Err(e) => {
                memory_warning = Some(format!(
                    "> WARN: failed to read `.agents/memory.md` ({}); proceeding with code map only.\n\n",
                    e
                ));
                None
            }
        }
    } else {
        None
    };

    let mut epics = None;
    let mut vocab_gotchas = None;
    let mut active_debt = Vec::new();

    if let Some(l) = &ledger {
        if !l.epics.trim().is_empty() {
            epics = Some(l.epics.trim().to_string());
        }
        if !l.vocab_gotchas.trim().is_empty() {
            vocab_gotchas = Some(l.vocab_gotchas.trim().to_string());
        }
        for (_, line) in &l.raw_debt_lines {
            active_debt.push(line.clone());
        }
    }

    let debt_count = active_debt.len();
    let adr_entries = index_adr_titles(workspace_root);
    let adrs: Vec<String> = adr_entries
        .iter()
        .map(|(path, title)| format!("`{}`: {}", path, title))
        .collect();

    // Reserve section floors so epics/vocab cannot starve the architectural map.
    let header_chars = 64usize;
    let usable_chars = char_budget.saturating_sub(header_chars);
    let map_floor = (usable_chars * 40) / 100;
    let debt_cap = (usable_chars * 25) / 100;
    let meta_cap = usable_chars.saturating_sub(map_floor);

    let mut meta_md = String::new();
    if let Some(warn) = &memory_warning {
        meta_md.push_str(warn);
    }
    if let Some(ep) = &epics {
        meta_md.push_str("## ACTIVE EPICS & SCALE\n");
        meta_md.push_str(ep);
        meta_md.push_str("\n\n");
    }
    if let Some(vg) = &vocab_gotchas {
        meta_md.push_str("## DOMAIN VOCABULARY & GOTCHAS\n");
        meta_md.push_str(vg);
        meta_md.push_str("\n\n");
    }
    if !adrs.is_empty() {
        meta_md.push_str("## ADRs (docs/adr)\n");
        for a in &adrs {
            meta_md.push_str(&format!("- {}\n", a));
        }
        meta_md.push('\n');
    }

    let mut debt_md = String::new();
    if !active_debt.is_empty() {
        debt_md.push_str(&format!("## ACTIVE DEBT ({}/30)\n", debt_count));
        for d in &active_debt {
            debt_md.push_str(d);
            debt_md.push('\n');
        }
        debt_md.push('\n');
    }

    // Cap metadata + debt if graph is present so map always retains >= 40% share.
    let (meta_fitted, debt_fitted) = if graph.is_some() {
        let debt_used = debt_md.chars().count().min(debt_cap);
        let meta_limit = meta_cap.saturating_sub(debt_used);
        (
            truncate_to_chars(&meta_md, meta_limit),
            truncate_to_chars(&debt_md, debt_cap),
        )
    } else {
        (meta_md, debt_md)
    };

    let prefix_chars = meta_fitted.chars().count() + debt_fitted.chars().count();
    let max_map_chars = usable_chars.saturating_sub(prefix_chars);

    let (map_result, map_md) = if let Some(g) = graph {
        let personalized = compute_personalized_scores(g, focus, None, workspace_root)?;
        let scores_ref = personalized
            .as_ref()
            .map(|(scores, stats)| (scores.as_slice(), stats));

        let symbol_limit = (max_map_chars / 90).clamp(5, 40);
        let res = generate_map_with_scores(g, scores_ref, None, focus, Some(symbol_limit));
        let md = res.format_compact_markdown();
        (Some(res), md)
    } else {
        (None, String::new())
    };

    let mut body = String::new();
    body.push_str(&meta_fitted);
    body.push_str(&debt_fitted);
    if !map_md.is_empty() {
        body.push_str("## ARCHITECTURAL MAP (PageRank Centrality)\n");
        body.push_str(&map_md);
    }

    let header_prefix = format!(
        "# MIMORI TURN-0 CONTEXT (tokens: ~XXXX / budget: {})\n\n",
        budget
    );
    let body_budget = char_budget.saturating_sub(header_prefix.chars().count());
    let fitted_body = truncate_to_chars(&body, body_budget);

    let est_with_header = |tok: usize| -> String {
        format!(
            "# MIMORI TURN-0 CONTEXT (tokens: ~{} / budget: {})\n\n{}",
            tok, budget, fitted_body
        )
    };
    let approx = est_with_header(budget).chars().count().div_ceil(4);
    let final_markdown = est_with_header(approx);
    let estimated_tokens = final_markdown.chars().count().div_ceil(4);
    let final_markdown = est_with_header(estimated_tokens);

    Ok(DumpResult {
        budget,
        estimated_tokens,
        epics,
        vocab_gotchas,
        active_debt,
        debt_count,
        adrs,
        map: map_result,
        markdown: final_markdown,
    })
}

fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let suffix = "... [truncated to fit token budget]\n";
    let suffix_len = suffix.chars().count();
    if max_chars <= suffix_len {
        return suffix.to_string();
    }
    let limit = max_chars - suffix_len;
    let mut out = String::new();
    let mut used = 0usize;
    for line in text.lines() {
        let len = line.chars().count() + 1;
        if used + len > limit {
            break;
        }
        out.push_str(line);
        out.push('\n');
        used += len;
    }
    out.push_str(suffix);
    out
}
