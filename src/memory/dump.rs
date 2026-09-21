use crate::graph::map::{generate_map, personalize_map, MapResult};
use crate::memory::ledger::MemoryLedger;
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
    let char_budget = budget * 4;

    // 1. Read memory ledger if exists
    let ledger = MemoryLedger::load(workspace_root).ok();

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
    let header_reserved = 150;
    let max_map_chars = char_budget.saturating_sub(vocab_debt_md.len() + header_reserved);

    // 2. Generate architectural map if graph is available
    let (map_result, map_md) = if let Ok(mut graph) = get_or_sync_graph(workspace_root) {
        if let Some(f) = focus {
            let _ = personalize_map(&mut graph, Some(f), None, workspace_root);
        }

        // Approx 100 chars per symbol in map
        let symbol_limit = (max_map_chars / 100).clamp(3, 40);
        let res = generate_map(&graph, None, focus, Some(symbol_limit));
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
    if body.len() + header_reserved > char_budget {
        let lines: Vec<&str> = body.lines().collect();
        let mut truncated = String::new();
        for line in lines {
            if truncated.len() + line.len() + 1 + header_reserved > char_budget {
                truncated.push_str("… [truncated to fit token budget]\n");
                break;
            }
            truncated.push_str(line);
            truncated.push('\n');
        }
        body = truncated;
    }

    let estimated_tokens = (body.len() + header_reserved).div_ceil(4);
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
