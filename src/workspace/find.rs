use crate::model::Symbol;
use crate::storage::get_or_sync_graph;
use crate::workspace::walker::WorkspaceScope;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MAX_LITERAL_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LITERAL_HITS: usize = 500;
const MAX_LITERAL_PREVIEW_CHARS: usize = 160;
const MAX_LITERAL_SIGNATURE_CHARS: usize = 240;

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

fn compact_preview(raw: &str, max_chars: usize) -> String {
    let compact: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        let head: String = compact.chars().take(max_chars).collect();
        format!("{}...", head)
    } else {
        compact
    }
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
                out.push_str(&format!("- **`{}`** (file)\n", m.file));
            } else if m.kind == "literal" {
                let preview = compact_preview(&m.signature, MAX_LITERAL_PREVIEW_CHARS);
                if m.coordinate.contains(":#L") {
                    out.push_str(&format!(
                        "- **`{}`** (literal) -> `{}` (L{})\n",
                        preview, m.file, m.start_line
                    ));
                } else {
                    out.push_str(&format!(
                        "- **`{}`** (literal) -> `{}` (L{}) [in `{}`]\n",
                        preview, m.file, m.start_line, m.coordinate
                    ));
                }
            } else {
                out.push_str(&format!(
                    "- **`{}`** ({}) [rank: {:.4}] -> `{}` (L{}-L{})\n",
                    m.name, m.kind, m.centrality, m.coordinate, m.start_line, m.end_line
                ));
                if idx < 15 && !m.signature.is_empty() {
                    out.push_str(&format!("    `{}`\n", compact_preview(&m.signature, 120)));
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

    parts
}

fn truncate_literal_snippet(line: &str, terms: &[&String], max_chars: usize) -> String {
    let trimmed = line.trim();
    let total_chars = trimmed.chars().count();
    if total_chars <= max_chars {
        return trimmed.to_string();
    }

    let lowered = trimmed.to_lowercase();
    let match_char_idx = terms
        .iter()
        .filter_map(|t| lowered.find(t.as_str()))
        .min()
        .map(|byte_idx| lowered[..byte_idx].chars().count().min(total_chars))
        .unwrap_or(0);

    let half = max_chars / 2;
    let max_start = total_chars.saturating_sub(max_chars);
    let start = match_char_idx.saturating_sub(half).min(max_start);
    let end = (start + max_chars).min(total_chars);

    let slice: String = trimmed.chars().skip(start).take(end - start).collect();
    let mut out = String::with_capacity(slice.len() + 6);
    if start > 0 {
        out.push_str("...");
    }
    out.push_str(&slice);
    if end < total_chars {
        out.push_str("...");
    }
    out
}

fn collect_literal_fallback(
    root: &Path,
    indexed_files: &[String],
    symbols: &[Symbol],
    parsed_scope: Option<&WorkspaceScope>,
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
            .map(|(rel, _, _, _)| rel)
            .collect()
    } else {
        indexed_files.to_vec()
    };

    let mut literal_count = 0usize;
    'files: for rel in &fallback_files {
        if parsed_scope.is_some_and(|sc| !sc.matches_rel(rel)) {
            continue;
        }
        let Ok(confined) = crate::workspace::walker::confine_to_workspace(root, Path::new(rel))
        else {
            continue;
        };
        let Ok(Some(bytes)) =
            crate::workspace::walker::read_bounded(&confined, MAX_LITERAL_FILE_BYTES)
        else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
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

            let coordinate = match enclosing {
                Some(s) => format!("{} (L{}-L{})", s.coordinate(), s.start_line, s.end_line),
                None => format!("{}:#L{}", rel, line_no),
            };

            matches.push(FindMatch {
                name: format!("{} (literal match)", rel),
                kind: "literal".to_string(),
                file: rel.clone(),
                coordinate,
                start_line: line_no,
                end_line: line_no,
                signature: truncate_literal_snippet(
                    line,
                    unmatched_terms,
                    MAX_LITERAL_SIGNATURE_CHARS,
                ),
                // Keep literal fallback at 0.0 so symbol declarations always outrank literals.
                centrality: 0.0,
            });
            literal_count += 1;
            if literal_count >= MAX_LITERAL_HITS {
                break 'files;
            }
        }
    }
}

fn validate_find_inputs(
    root: &Path,
    query: &str,
    symbols_only: bool,
    files_only: bool,
    scope: Option<&str>,
) -> Result<(Vec<String>, Option<WorkspaceScope>)> {
    if query.trim().is_empty() {
        bail!("Find pattern cannot be empty.");
    }
    if symbols_only && files_only {
        bail!("Cannot combine --symbols-only and --files-only.");
    }
    let q_terms = split_query_terms(query);
    if q_terms.is_empty() {
        bail!("Find pattern must contain at least one non-empty term.");
    }
    let parsed_scope = match scope {
        Some(sc) => Some(WorkspaceScope::parse(root, sc, false)?),
        None => None,
    };
    Ok((q_terms, parsed_scope))
}

fn collect_file_matches(
    graph: &crate::graph::SymbolGraph,
    q_terms: &[String],
    parsed_scope: Option<&WorkspaceScope>,
    matches: &mut Vec<FindMatch>,
) {
    let mut seen_files: HashSet<&str> = HashSet::new();
    let iter_indexed = graph.indexed_files.iter().map(String::as_str);
    let iter_syms = graph.symbols.iter().map(|s| s.file.as_str());

    for path in iter_indexed.chain(iter_syms) {
        if !seen_files.insert(path) || parsed_scope.is_some_and(|sc| !sc.matches_rel(path)) {
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

fn collect_symbol_matches(
    graph: &crate::graph::SymbolGraph,
    q_terms: &[String],
    parsed_scope: Option<&WorkspaceScope>,
    matches: &mut Vec<FindMatch>,
) {
    for s in &graph.symbols {
        if parsed_scope.is_some_and(|sc| !sc.matches_rel(&s.file)) {
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

fn find_unmatched_terms<'a>(q_terms: &'a [String], matches: &[FindMatch]) -> Vec<&'a String> {
    let match_lowers: Vec<(String, String)> = matches
        .iter()
        .map(|m| {
            let file_l = if m.kind == "file" {
                m.file.to_lowercase()
            } else {
                String::new()
            };
            (m.name.to_lowercase(), file_l)
        })
        .collect();

    q_terms
        .iter()
        .filter(|t| {
            !match_lowers.iter().any(|(nl, fl)| {
                nl.contains(t.as_str()) || (!fl.is_empty() && fl.contains(t.as_str()))
            })
        })
        .collect()
}

fn sort_and_truncate_matches(
    matches: Vec<FindMatch>,
    q_terms: &[String],
    limit: Option<usize>,
) -> (Vec<FindMatch>, Option<usize>) {
    let mut keyed: Vec<(bool, bool, FindMatch)> = matches
        .into_iter()
        .map(|m| {
            let exact = q_terms.contains(&m.name.to_lowercase())
                || (m.kind == "file" && q_terms.contains(&m.file.to_lowercase()));
            let is_decl = m.kind != "literal";
            (exact, is_decl, m)
        })
        .collect();

    keyed.sort_by(|(a_exact, a_decl, a), (b_exact, b_decl, b)| {
        b_exact
            .cmp(a_exact)
            .then_with(|| b_decl.cmp(a_decl))
            .then_with(|| b.centrality.total_cmp(&a.centrality))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut sorted: Vec<FindMatch> = keyed.into_iter().map(|(_, _, m)| m).collect();
    let total_matches = sorted.len();
    let truncated_from = match limit {
        Some(n) if n < total_matches => {
            sorted.truncate(n);
            Some(total_matches)
        }
        _ => None,
    };
    (sorted, truncated_from)
}

#[allow(clippy::too_many_arguments)]
fn execute_find_parsed(
    root: &Path,
    graph: &crate::graph::SymbolGraph,
    query: &str,
    q_terms: &[String],
    symbols_only: bool,
    files_only: bool,
    parsed_scope: Option<&WorkspaceScope>,
    limit: Option<usize>,
) -> FindResult {
    let mut matches = Vec::new();
    if !symbols_only {
        collect_file_matches(graph, q_terms, parsed_scope, &mut matches);
    }
    if !files_only {
        collect_symbol_matches(graph, q_terms, parsed_scope, &mut matches);
    }
    if !files_only && !symbols_only {
        let unmatched_terms = find_unmatched_terms(q_terms, &matches);
        if !unmatched_terms.is_empty() {
            collect_literal_fallback(
                root,
                &graph.indexed_files,
                &graph.symbols,
                parsed_scope,
                &unmatched_terms,
                &mut matches,
            );
        }
    }

    let (matches, truncated_from) = sort_and_truncate_matches(matches, q_terms, limit);
    FindResult {
        query: query.to_string(),
        matches,
        truncated_from,
    }
}

pub fn execute_find(
    root: &Path,
    query: &str,
    symbols_only: bool,
    files_only: bool,
    scope: Option<&str>,
    limit: Option<usize>,
) -> Result<FindResult> {
    let (q_terms, parsed_scope) =
        validate_find_inputs(root, query, symbols_only, files_only, scope)?;
    let graph = get_or_sync_graph(root)?;
    Ok(execute_find_parsed(
        root,
        &graph,
        query,
        &q_terms,
        symbols_only,
        files_only,
        parsed_scope.as_ref(),
        limit,
    ))
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
    let (q_terms, parsed_scope) =
        validate_find_inputs(root, query, symbols_only, files_only, scope)?;
    Ok(execute_find_parsed(
        root,
        graph,
        query,
        &q_terms,
        symbols_only,
        files_only,
        parsed_scope.as_ref(),
        limit,
    ))
}
