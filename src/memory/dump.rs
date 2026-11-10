use crate::graph::map::{compute_personalized_scores, generate_map_with_scores, MapResult};
use crate::graph::SymbolGraph;
use crate::memory::ledger::{confine_agents_path, MemoryLedger};
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
    pub map: Option<MapResult>,
    pub markdown: String,
}

pub fn generate_dump(
    workspace_root: &Path,
    budget: usize,
    focus: Option<&str>,
) -> Result<DumpResult> {
    let graph = get_or_sync_graph(workspace_root).ok();
    generate_dump_with_graph(workspace_root, graph.as_ref(), budget, focus)
}

pub fn generate_dump_with_graph(
    workspace_root: &Path,
    graph: Option<&SymbolGraph>,
    budget: usize,
    focus: Option<&str>,
) -> Result<DumpResult> {
    let char_budget = budget.saturating_mul(4);

    // 1. Read memory ledger if exists (fail closed if .agents escapes workspace)
    let mem_path = confine_agents_path(workspace_root, ".agents/memory.md")?;
    let ledger = if mem_path.exists() {
        Some(MemoryLedger::load(workspace_root)?)
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

    // Estimate chars for epics, vocab and debt
    let mut vocab_debt_md = String::new();

    if let Some(ep) = &epics {
        vocab_debt_md.push_str("## ACTIVE EPICS & SCALE\n");
        vocab_debt_md.push_str(ep);
        vocab_debt_md.push_str("\n\n");
    }

    if let Some(vg) = &vocab_gotchas {
        vocab_debt_md.push_str("## DOMAIN VOCABULARY & GOTCHAS\n");
        vocab_debt_md.push_str(vg);
        vocab_debt_md.push_str("\n\n");
    }

    if !active_debt.is_empty() {
        vocab_debt_md.push_str(&format!("## ACTIVE DEBT ({}/30)\n", debt_count));
        for d in &active_debt {
            vocab_debt_md.push_str(d);
            vocab_debt_md.push('\n');
        }
        vocab_debt_md.push('\n');
    }

    // Reserve 150 chars for top header
    let header_reserved = 150usize;
    let max_map_chars = char_budget
        .saturating_sub(vocab_debt_md.chars().count())
        .saturating_sub(header_reserved);

    // 2. Generate architectural map if graph is available (zero SymbolGraph clones)
    let (map_result, map_md) = if let Some(g) = graph {
        let personalized = compute_personalized_scores(g, focus, None, workspace_root)?;
        let scores_ref = personalized
            .as_ref()
            .map(|(scores, stats)| (scores.as_slice(), stats));

        // Approx 100 chars per symbol in map
        let symbol_limit = (max_map_chars / 100).clamp(3, 40);
        let res = generate_map_with_scores(g, scores_ref, None, focus, Some(symbol_limit));
        let md = format_map_section(&res);
        (Some(res), md)
    } else {
        (None, String::new())
    };

    // 3. Assemble sections within budget
    let mut body = String::new();
    body.push_str(&vocab_debt_md);

    if !map_md.is_empty() {
        body.push_str("## ARCHITECTURAL MAP (PageRank Centrality)\n");
        body.push_str(&map_md);
    }

    // Truncate body line by line if exceeding budget
    let mut body_chars = body.chars().count();
    if body_chars.saturating_add(header_reserved) > char_budget {
        let mut truncated = String::new();
        let mut trunc_chars = 0usize;
        for line in body.lines() {
            let line_chars = line.chars().count() + 1;
            if trunc_chars
                .saturating_add(line_chars)
                .saturating_add(header_reserved)
                > char_budget
            {
                let suffix = "… [truncated to fit token budget]\n";
                truncated.push_str(suffix);
                trunc_chars = trunc_chars.saturating_add(suffix.chars().count());
                break;
            }
            truncated.push_str(line);
            truncated.push('\n');
            trunc_chars = trunc_chars.saturating_add(line_chars);
        }
        body = truncated;
        body_chars = trunc_chars;
    }

    let estimated_tokens = body_chars.saturating_add(header_reserved).div_ceil(4);
    let final_markdown = format!(
        "# MIMORI TURN-0 CONTEXT (tokens: ~{} / budget: {})\n\n{}",
        estimated_tokens, budget, body
    );

    Ok(DumpResult {
        budget,
        estimated_tokens,
        epics,
        vocab_gotchas,
        active_debt,
        debt_count,
        map: map_result,
        markdown: final_markdown,
    })
}

fn format_map_section(map: &MapResult) -> String {
    let mut out = String::new();
    if map.modules.is_empty() {
        out.push_str("No indexed symbols found in workspace.\n\n");
        return out;
    }

    for module in &map.modules {
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
