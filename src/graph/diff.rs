use crate::graph::blast::{traverse_multi_upstream, BlastNode};
use crate::graph::SymbolGraph;
use crate::memory::{check_debt, MemoryLedger};
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

pub fn validate_diff_base(base: Option<&str>) -> Result<()> {
    if let Some(b) = base {
        if b.trim().is_empty() || b.starts_with('-') || b.chars().any(|c| c.is_control()) {
            bail!(
                "Invalid diff base revision '{}': must not be empty, start with '-', or contain control characters",
                b
            );
        }
    }
    Ok(())
}

pub fn calculate_diff_blast(
    root: &Path,
    graph: &SymbolGraph,
    staged: bool,
    base: Option<&str>,
    depth: usize,
) -> Result<DiffBlastReport> {
    validate_diff_base(base)?;

    let mut cmd = Command::new("git");
    cmd.current_dir(root).stdin(Stdio::null()).args([
        "-c",
        "core.quotepath=false",
        "diff",
        "--relative",
        "--no-color",
        "--no-ext-diff",
        "-U0",
    ]);
    if staged {
        cmd.arg("--staged");
    }
    if let Some(b) = base {
        cmd.arg(b);
    }
    cmd.arg("--");

    let output = cmd.output().context("Failed to execute 'git diff'")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff exited with error: {}", stderr.trim());
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    let hunks = parse_unified_diff_detailed(&diff_text);

    let mut touched_files = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();
    for h in &hunks {
        if seen_files.insert(h.file.clone()) {
            touched_files.push(h.file.clone());
        }
    }

    let mut syms_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, sym) in graph.symbols.iter().enumerate() {
        if seen_files.contains(sym.file.as_str()) {
            syms_by_file.entry(sym.file.as_str()).or_default().push(idx);
        }
    }

    let mut mutated_indices = Vec::new();
    let mut seen_mutated = HashSet::new();
    for h in &hunks {
        let Some(file_syms) = syms_by_file.get(h.file.as_str()) else {
            continue;
        };
        for &idx in file_syms {
            let sym = &graph.symbols[idx];
            let overlaps = if h.is_deletion {
                // Pure deletion (`+anchor,0`): lines were removed immediately
                // after `h.anchor_start` in the new file. Deleting lines *after*
                // `sym.end_line` (e.g. a sibling function below `sym`) does not
                // mutate `sym`.
                h.start > 0 && sym.start_line <= h.start && h.start < sym.end_line
            } else {
                sym.start_line <= h.end && sym.end_line >= h.start
            };
            if overlaps && seen_mutated.insert(idx) {
                mutated_indices.push(idx);
            }
        }
    }
    mutated_indices.sort_unstable();

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

    let (affected, entry_points, test_suites, literal_test_mentions) =
        traverse_multi_upstream(graph, &mutated_indices, depth);

    let (_debt_ok, debt_status) = check_debt(root);
    let memory_status = match crate::memory::ledger::confine_agents_path(root, ".agents/memory.md")
    {
        Ok(p) if p.exists() => match MemoryLedger::load(root) {
            Ok(ledger) => ledger.lint().to_m2m_output(),
            Err(e) => format!("MEM_ERR: {}", e),
        },
        Ok(_) => "MEM_SKIP: no .agents/memory.md present.".to_string(),
        Err(e) => format!("MEM_ERR: {}", e),
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

struct DiffHunk {
    file: String,
    start: usize,
    end: usize,
    is_deletion: bool,
}

fn unquote_diff_path(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn parse_unified_diff_detailed(diff_text: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current_file: Option<String> = None;

    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let trimmed = unquote_diff_path(rest);
            if trimmed == "/dev/null" {
                current_file = None;
            } else if let Some(rel) = trimmed.strip_prefix("b/") {
                current_file = Some(rel.to_string());
            } else {
                current_file = Some(trimmed.to_string());
            }
        } else if line.starts_with("@@ ") {
            if let (Some(file), Some((start, end, is_deletion))) =
                (&current_file, parse_hunk_header_detailed(line))
            {
                hunks.push(DiffHunk {
                    file: file.clone(),
                    start,
                    end,
                    is_deletion,
                });
            }
        }
    }
    hunks
}

pub fn parse_unified_diff(diff_text: &str) -> Vec<(String, usize, usize)> {
    parse_unified_diff_detailed(diff_text)
        .into_iter()
        .map(|h| (h.file, h.start.max(1), h.end.max(1)))
        .collect()
}

fn parse_hunk_header_detailed(line: &str) -> Option<(usize, usize, bool)> {
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
        Some((start, start, true))
    } else {
        let s = start.max(1);
        Some((s, s + count - 1, false))
    }
}

pub fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let (start, end, is_deletion) = parse_hunk_header_detailed(line)?;
    if is_deletion {
        let anchor = start.max(1);
        Some((anchor, anchor))
    } else {
        Some((start, end))
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
