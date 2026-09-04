use crate::storage::get_or_sync_graph;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindMatch {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub centrality: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResult {
    pub query: String,
    pub matches: Vec<FindMatch>,
}

impl FindResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let is_literal_fallback = self.matches.iter().any(|m| m.kind == "literal");
        if is_literal_fallback {
            out.push_str(&format!(
                "### Find: `{}` ({} literal matches) [fallback]\n\n",
                self.query,
                self.matches.len()
            ));
        } else {
            out.push_str(&format!(
                "### Find: `{}` ({} matches)\n\n",
                self.query,
                self.matches.len()
            ));
        }

        if self.matches.is_empty() {
            out.push_str("No matches found.\n");
            return out;
        }

        for m in &self.matches {
            if m.kind == "file" {
                out.push_str(&format!("- 📄 **`{}`** (file)\n", m.file));
            } else if m.kind == "literal" {
                out.push_str(&format!(
                    "- 🔍 **`{}`** (literal) → `{}` (L{})\n",
                    m.signature, m.file, m.start_line
                ));
            } else {
                out.push_str(&format!(
                    "- 🔹 **`{}`** ({}) [rank: {:.4}] → `{}` (L{}-L{})\n",
                    m.name, m.kind, m.centrality, m.coordinate, m.start_line, m.end_line
                ));
                if !m.signature.is_empty() {
                    out.push_str(&format!("    `{}`\n", m.signature));
                }
            }
        }

        out
    }
}

pub fn execute_find(
    root: &Path,
    query: &str,
    symbols_only: bool,
    files_only: bool,
) -> Result<FindResult> {
    let graph = get_or_sync_graph(root)?;
    let q_lower = query.to_lowercase();
    let mut matches = Vec::new();

    if !symbols_only {
        let mut seen_files = HashSet::new();
        for s in &graph.symbols {
            if !seen_files.contains(&s.file) {
                seen_files.insert(s.file.clone());
                if s.file.to_lowercase().contains(&q_lower) {
                    matches.push(FindMatch {
                        name: Path::new(&s.file)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        kind: "file".to_string(),
                        file: s.file.clone(),
                        coordinate: s.file.clone(),
                        start_line: 1,
                        end_line: 1,
                        signature: String::new(),
                        centrality: 0.0,
                    });
                }
            }
        }
    }

    if !files_only {
        for s in &graph.symbols {
            if s.name.to_lowercase().contains(&q_lower) {
                matches.push(FindMatch {
                    name: s.name.clone(),
                    kind: s.kind.as_str().to_string(),
                    file: s.file.clone(),
                    coordinate: s.coordinate(),
                    start_line: s.start_line,
                    end_line: s.end_line,
                    signature: s.signature.clone(),
                    centrality: s.centrality,
                });
            }
        }
    }

    // Hybrid fallback: If 0 AST symbols or files matched, scan symbol bodies for literal matches
    if matches.is_empty() {
        for s in &graph.symbols {
            let body_lower = s.body.to_lowercase();
            if let Some((line_offset, line_text)) = locate_literal(&s.body, &body_lower, &q_lower) {
                let match_line = s.start_line + line_offset;

                matches.push(FindMatch {
                    name: format!("{} (literal match)", s.name),
                    kind: "literal".to_string(),
                    file: s.file.clone(),
                    coordinate: format!("{}:#L{}", s.file, match_line),
                    start_line: match_line,
                    end_line: match_line,
                    signature: line_text.to_string(),
                    centrality: s.centrality,
                });
            }
        }
    }

    matches.sort_by(|a, b| {
        let a_exact = a.name.to_lowercase() == q_lower;
        let b_exact = b.name.to_lowercase() == q_lower;
        b_exact
            .cmp(&a_exact)
            .then_with(|| b.centrality.partial_cmp(&a.centrality).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.name.len().cmp(&b.name.len()))
    });

    Ok(FindResult {
        query: query.to_string(),
        matches,
    })
}

/// Locate `needle` (already lowercased) within `body_lower`, returning the
/// 0-based line offset of the match and that line's text from the original
/// `body`.
///
/// `body_lower` is indexed rather than `body` because lowercasing is not
/// byte-length preserving (e.g. 'İ' grows from 2 bytes to 3). Case mapping
/// never adds or removes newlines, so the line offset is valid in both.
fn locate_literal<'a>(body: &'a str, body_lower: &str, needle: &str) -> Option<(usize, &'a str)> {
    let pos = body_lower.find(needle)?;
    let line_offset = body_lower[..pos].matches('\n').count();
    let line_text = body.lines().nth(line_offset).unwrap_or("").trim();
    Some((line_offset, line_text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_literal_handles_multibyte_case_expansion() {
        // 'İ' is 2 bytes but lowercases to 3, so a byte offset taken from the
        // lowered string is invalid in the original. Regression: M3/find.rs.
        let body = "fn f() {\n    // İİİİİİİİİİ marker\n    \"İstanbul\"\n}";
        let lower = body.to_lowercase();
        assert!(lower.len() > body.len(), "test needs case expansion");

        let (offset, text) = locate_literal(body, &lower, "marker").unwrap();
        assert_eq!(offset, 1, "marker is on the second line");
        assert!(text.contains("marker"), "got: {text}");

        // Must not panic even when the offset lands near the end.
        let (offset, text) = locate_literal(body, &lower, "stanbul").unwrap();
        assert_eq!(offset, 2);
        assert!(text.contains("stanbul"), "got: {text}");
    }

    #[test]
    fn locate_literal_returns_none_when_absent() {
        let body = "fn f() {}";
        assert!(locate_literal(body, &body.to_lowercase(), "zzz").is_none());
    }
}
