use crate::model::Symbol;
use crate::storage::get_or_sync_graph;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_from: Option<usize>,
}

impl FindResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let is_literal_fallback =
            !self.matches.is_empty() && self.matches.iter().all(|m| m.kind == "literal");
        if is_literal_fallback {
            out.push_str(&format!(
                "### Find: `{}` ({} literal matches",
                self.query,
                self.matches.len()
            ));
        } else {
            out.push_str(&format!(
                "### Find: `{}` ({} matches",
                self.query,
                self.matches.len()
            ));
        }

        if let Some(total) = self.truncated_from {
            out.push_str(&format!(" of {} [--limit]", total));
        }

        if is_literal_fallback {
            out.push_str(") [fallback]\n\n");
        } else {
            out.push_str(")\n\n");
        }

        if self.matches.is_empty() {
            out.push_str("No matches found.\n");
            return out;
        }

        for (idx, m) in self.matches.iter().enumerate() {
            if m.kind == "file" {
                out.push_str(&format!("- 📄 **`{}`** (file)\n", m.file));
            } else if m.kind == "literal" {
                if m.coordinate.contains(":#L") {
                    out.push_str(&format!(
                        "- 🔍 **`{}`** (literal) → `{}` (L{})\n",
                        m.signature, m.file, m.start_line
                    ));
                } else {
                    out.push_str(&format!(
                        "- 🔍 **`{}`** (literal) → `{}` (L{}) [in `{}`]\n",
                        m.signature, m.file, m.start_line, m.coordinate
                    ));
                }
            } else {
                out.push_str(&format!(
                    "- 🔹 **`{}`** ({}) [rank: {:.4}] → `{}` (L{}-L{})\n",
                    m.name, m.kind, m.centrality, m.coordinate, m.start_line, m.end_line
                ));
                if idx < 15 && !m.signature.is_empty() {
                    let compact_sig: String =
                        m.signature.split_whitespace().collect::<Vec<_>>().join(" ");
                    let preview = if compact_sig.chars().count() > 120 {
                        let head: String = compact_sig.chars().take(120).collect();
                        format!("{}…", head)
                    } else {
                        compact_sig
                    };
                    out.push_str(&format!("    `{}`\n", preview));
                }
            }
        }

        out
    }
}

fn split_query_terms(query: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;

    for ch in query.chars() {
        match ch {
            '<' | '(' | '[' | '{' => {
                depth += 1;
                current.push(ch);
            }
            '>' | ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            '|' => {
                let trimmed = current.trim().to_lowercase();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current.clear();
                depth = 0;
            }
            ',' if depth == 0 => {
                let trimmed = current.trim().to_lowercase();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let tail = current.trim().to_lowercase();
    if !tail.is_empty() {
        parts.push(tail);
    }

    if parts.is_empty() {
        vec![query.to_lowercase()]
    } else {
        parts
    }
}

fn collect_literal_fallback(
    root: &Path,
    indexed_files: &[String],
    symbols: &[Symbol],
    norm_scope: Option<&str>,
    unmatched_terms: &[&String],
    matches: &mut Vec<FindMatch>,
) {
    let mut syms_by_file: HashMap<&str, Vec<&Symbol>> = HashMap::new();
    for s in symbols {
        syms_by_file.entry(s.file.as_str()).or_default().push(s);
    }

    let fallback_files: Vec<String> = if indexed_files.is_empty() {
        crate::workspace::walker::discover_workspace(root)
            .supported
            .into_iter()
            .map(|(rel, _, _)| rel)
            .collect()
    } else {
        indexed_files.to_vec()
    };

    for rel in &fallback_files {
        if !matches_scope(rel, norm_scope) {
            continue;
        }
        let Ok(confined) = crate::workspace::walker::confine_to_workspace(root, Path::new(rel))
        else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&confined) else {
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        let file_syms = syms_by_file.get(rel.as_str());
        for (idx, line) in content.lines().enumerate() {
            let line_lower = line.to_lowercase();
            if !unmatched_terms
                .iter()
                .any(|t| line_lower.contains(t.as_str()))
            {
                continue;
            }
            let line_no = idx + 1;
            let enclosing = file_syms.and_then(|list| {
                list.iter()
                    .copied()
                    .filter(|s| s.start_line <= line_no && line_no <= s.end_line)
                    .min_by_key(|s| s.end_line.saturating_sub(s.start_line))
            });

            let (coordinate, centrality) = match enclosing {
                Some(s) => (
                    format!("{} (L{}-L{})", s.coordinate(), s.start_line, s.end_line),
                    s.centrality,
                ),
                None => (format!("{}:#L{}", rel, line_no), 0.0),
            };

            matches.push(FindMatch {
                name: format!("{} (literal match)", rel),
                kind: "literal".to_string(),
                file: rel.clone(),
                coordinate,
                start_line: line_no,
                end_line: line_no,
                signature: line.trim().to_string(),
                centrality,
            });
        }
    }
}

fn normalize_scope(scope: Option<&str>) -> Option<String> {
    let raw = scope?.trim();
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<String> = Path::new(raw)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn matches_scope(file: &str, norm_scope: Option<&str>) -> bool {
    let Some(sc) = norm_scope else {
        return true;
    };
    file == sc
        || (file.len() > sc.len() && file.starts_with(sc) && file.as_bytes()[sc.len()] == b'/')
}

pub fn execute_find(
    root: &Path,
    query: &str,
    symbols_only: bool,
    files_only: bool,
    scope: Option<&str>,
    limit: Option<usize>,
) -> Result<FindResult> {
    let graph = get_or_sync_graph(root)?;
    execute_find_with_graph(root, &graph, query, symbols_only, files_only, scope, limit)
}

pub fn execute_find_with_graph(
    root: &Path,
    graph: &crate::graph::SymbolGraph,
    query: &str,
    symbols_only: bool,
    files_only: bool,
    scope: Option<&str>,
    limit: Option<usize>,
) -> Result<FindResult> {
    let q_terms = split_query_terms(query);
    let norm_scope = normalize_scope(scope);
    let mut matches = Vec::new();

    if !symbols_only {
        let mut seen_files: HashSet<&str> = HashSet::new();
        let iter_indexed = graph.indexed_files.iter().map(|s| s.as_str());
        let iter_syms = graph.symbols.iter().map(|s| s.file.as_str());

        for path in iter_indexed.chain(iter_syms) {
            if !seen_files.insert(path) {
                continue;
            }
            if !matches_scope(path, norm_scope.as_deref()) {
                continue;
            }
            let path_lower = path.to_lowercase();
            if q_terms.iter().any(|t| path_lower.contains(t)) {
                matches.push(FindMatch {
                    name: Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    kind: "file".to_string(),
                    file: path.to_string(),
                    coordinate: path.to_string(),
                    start_line: 1,
                    end_line: 1,
                    signature: String::new(),
                    centrality: 0.0,
                });
            }
        }
    }

    if !files_only {
        for s in &graph.symbols {
            if !matches_scope(&s.file, norm_scope.as_deref()) {
                continue;
            }
            let name_lower = s.name.to_lowercase();
            if q_terms.iter().any(|t| name_lower.contains(t)) {
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

    // Precompute lowercase sort/filter keys once per match instead of inside O(N log N) sort_by.
    let match_lowers: Vec<(String, String)> = matches
        .iter()
        .map(|m| {
            (
                m.name.to_lowercase(),
                if m.kind == "file" {
                    m.file.to_lowercase()
                } else {
                    String::new()
                },
            )
        })
        .collect();

    if !files_only && !symbols_only {
        let unmatched_terms: Vec<&String> = q_terms
            .iter()
            .filter(|t| {
                !match_lowers.iter().any(|(nl, fl)| {
                    nl.contains(t.as_str()) || (!fl.is_empty() && fl.contains(t.as_str()))
                })
            })
            .collect();

        if !unmatched_terms.is_empty() {
            collect_literal_fallback(
                root,
                &graph.indexed_files,
                &graph.symbols,
                norm_scope.as_deref(),
                &unmatched_terms,
                &mut matches,
            );
        }
    }

    let mut keyed: Vec<(bool, FindMatch)> = matches
        .into_iter()
        .map(|m| {
            let exact = q_terms.contains(&m.name.to_lowercase());
            (exact, m)
        })
        .collect();
    keyed.sort_by(|(a_exact, a), (b_exact, b)| {
        b_exact
            .cmp(a_exact)
            .then_with(|| b.centrality.total_cmp(&a.centrality))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut matches: Vec<FindMatch> = keyed.into_iter().map(|(_, m)| m).collect();

    let total_matches = matches.len();
    let truncated_from = match limit {
        Some(n) if n < total_matches => {
            matches.truncate(n);
            Some(total_matches)
        }
        _ => None,
    };

    Ok(FindResult {
        query: query.to_string(),
        matches,
        truncated_from,
    })
}
