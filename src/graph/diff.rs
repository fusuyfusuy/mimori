use crate::graph::blast::{calculate_blast_radius, BlastNode};
use crate::graph::SymbolGraph;
use crate::memory::{check_debt, MemoryLedger};
use crate::model::Coordinate;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutatedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffBlastReport {
    pub touched_files: Vec<String>,
    pub mutated_symbols: Vec<MutatedSymbol>,
    pub affected: Vec<BlastNode>,
    pub entry_points: Vec<String>,
    pub test_suites: Vec<String>,
    pub literal_test_mentions: Vec<String>,
    pub debt_status: String,
    pub memory_status: String,
}

impl DiffBlastReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Diff Blast Report (Touched Files: {}, Mutated Symbols: {}, Upstream Affected: {})\n\n",
            self.touched_files.len(),
            self.mutated_symbols.len(),
            self.affected.len()
        ));

        if self.touched_files.is_empty() {
            out.push_str("Working tree diff is clean (0 changed hunks).\n\n");
        } else {
            out.push_str("### ✏️ Mutated Symbols\n");
            if self.mutated_symbols.is_empty() {
                out.push_str("No indexed symbols overlapped the diff hunks.\n");
            } else {
                for m in &self.mutated_symbols {
                    out.push_str(&format!(
                        "- **`{}`** ({}) → `{}` (L{}-L{})\n",
                        m.name, m.kind, m.coordinate, m.start_line, m.end_line
                    ));
                }
            }
            out.push('\n');
        }

        if !self.entry_points.is_empty() {
            out.push_str("### 🚪 Affected Entry Points\n");
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
            out.push_str("### 🌊 Combined Upstream Blast Cone (External to Diff)\n");
            for node in &self.affected {
                let indent = "  ".repeat(node.depth);
                let tag = if node.is_entry_point {
                    " [Entry Point]"
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
            out.push('\n');
        }

        if !self.literal_test_mentions.is_empty() {
            out.push_str("### 🔍 Literal / Source-Inspection Test Mentions\n");
            for m in &self.literal_test_mentions {
                out.push_str(&format!("- 🔍 `{}`\n", m));
            }
            out.push('\n');
        }

        out.push_str("### 🛡️ Debt & Memory Gate\n");
        out.push_str(&format!("- **Debt**: {}\n", self.debt_status.trim()));
        out.push_str(&format!("- **Memory**: {}\n", self.memory_status.trim()));
        out
    }
}

pub fn calculate_diff_blast(
    root: &Path,
    graph: &SymbolGraph,
    staged: bool,
    base: Option<&str>,
    depth: usize,
) -> Result<DiffBlastReport> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .args(["diff", "--no-color", "--no-ext-diff", "-U0"]);
    if staged {
        cmd.arg("--staged");
    }
    if let Some(b) = base {
        cmd.arg(b);
    }

    let output = cmd.output().context("Failed to execute 'git diff'")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff exited with error: {}", stderr.trim());
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    let hunks = parse_unified_diff(&diff_text);

    let mut touched_files = Vec::new();
    let mut seen_files = HashSet::new();
    for (file, _, _) in &hunks {
        if seen_files.insert(file.clone()) {
            touched_files.push(file.clone());
        }
    }

    let mut mutated_indices = Vec::new();
    let mut seen_mutated = HashSet::new();
    for (file, h_start, h_end) in &hunks {
        for (idx, sym) in graph.symbols.iter().enumerate() {
            if &sym.file == file
                && sym.start_line <= *h_end
                && sym.end_line >= *h_start
                && seen_mutated.insert(idx)
            {
                mutated_indices.push(idx);
            }
        }
    }

    let mutated_coords: HashSet<String> = mutated_indices
        .iter()
        .map(|&i| graph.symbols[i].coordinate())
        .collect();

    let mutated_symbols: Vec<MutatedSymbol> = mutated_indices
        .iter()
        .map(|&i| {
            let s = &graph.symbols[i];
            MutatedSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                file: s.file.clone(),
                coordinate: s.coordinate(),
                start_line: s.start_line,
                end_line: s.end_line,
            }
        })
        .collect();

    let mut best_affected: HashMap<String, BlastNode> = HashMap::new();
    let mut entry_points = Vec::new();
    let mut test_suites = Vec::new();
    let mut literal_test_mentions = Vec::new();

    for m in &mutated_symbols {
        let Ok(coord) = Coordinate::parse(&m.coordinate) else {
            continue;
        };
        let Ok(blast) = calculate_blast_radius(graph, &coord, depth) else {
            continue;
        };
        for ep in blast.entry_points {
            if !entry_points.contains(&ep) {
                entry_points.push(ep);
            }
        }
        for ts in blast.test_suites {
            if !test_suites.contains(&ts) {
                test_suites.push(ts);
            }
        }
        for lit in blast.literal_test_mentions {
            if !literal_test_mentions.contains(&lit) {
                literal_test_mentions.push(lit);
            }
        }
        for node in blast.affected {
            if mutated_coords.contains(&node.coordinate) {
                continue;
            }
            match best_affected.get_mut(&node.coordinate) {
                Some(existing) => {
                    if node.depth < existing.depth {
                        existing.depth = node.depth;
                    }
                    for l in node.call_lines {
                        if !existing.call_lines.contains(&l) {
                            existing.call_lines.push(l);
                        }
                    }
                }
                None => {
                    best_affected.insert(node.coordinate.clone(), node);
                }
            }
        }
    }

    let mut affected: Vec<BlastNode> = best_affected.into_values().collect();
    affected.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.coordinate.cmp(&b.coordinate))
    });

    let (_debt_ok, debt_status) = check_debt(root);
    let memory_status = if MemoryLedger::memory_path(root).exists() {
        match MemoryLedger::load(root) {
            Ok(ledger) => ledger.lint().to_m2m_output(),
            Err(e) => format!("MEM_ERR: {}", e),
        }
    } else {
        "MEM_SKIP: no .agents/memory.md present.".to_string()
    };

    Ok(DiffBlastReport {
        touched_files,
        mutated_symbols,
        affected,
        entry_points,
        test_suites,
        literal_test_mentions,
        debt_status,
        memory_status,
    })
}

pub fn parse_unified_diff(diff_text: &str) -> Vec<(String, usize, usize)> {
    let mut hunks = Vec::new();
    let mut current_file: Option<String> = None;

    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let trimmed = rest.trim();
            if trimmed == "/dev/null" {
                current_file = None;
            } else if let Some(rel) = trimmed.strip_prefix("b/") {
                current_file = Some(rel.to_string());
            } else {
                current_file = Some(trimmed.to_string());
            }
        } else if line.starts_with("@@ ") {
            if let (Some(file), Some((start, end))) = (&current_file, parse_hunk_header(line)) {
                hunks.push((file.clone(), start, end));
            }
        }
    }
    hunks
}

pub fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let body = line.strip_prefix("@@ ")?;
    let (ranges, _) = body.split_once(" @@")?;
    let plus_part = ranges
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix('+'))?;
    let (start_str, count_str) = match plus_part.split_once(',') {
        Some((s, c)) => (s, Some(c)),
        None => (plus_part, None),
    };
    let start: usize = start_str.parse().ok()?;
    let count: usize = match count_str {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    if count == 0 {
        let anchor = start.max(1);
        Some((anchor, anchor))
    } else {
        let s = start.max(1);
        Some((s, s + count - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_header_handles_single_line_multi_line_and_deletions() {
        assert_eq!(parse_hunk_header("@@ -10 +12 @@ fn foo"), Some((12, 12)));
        assert_eq!(parse_hunk_header("@@ -10,3 +20,5 @@"), Some((20, 24)));
        assert_eq!(parse_hunk_header("@@ -45,4 +44,0 @@"), Some((44, 44)));
    }
}
