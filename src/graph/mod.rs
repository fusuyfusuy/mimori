pub mod blast;
pub mod map;
pub mod pagerank;

use crate::model::{SliceResult, Symbol};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SymbolGraph {
    pub symbols: Vec<Symbol>,
    pub callers_map: HashMap<usize, Vec<usize>>,
    pub callees_map: HashMap<usize, Vec<usize>>,
}

impl SymbolGraph {
    pub fn new(mut symbols: Vec<Symbol>) -> Self {
        let mut name_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
        let mut coord_to_index: HashMap<String, usize> = HashMap::new();

        for (idx, sym) in symbols.iter().enumerate() {
            name_to_indices
                .entry(sym.name.clone())
                .or_default()
                .push(idx);
            coord_to_index.insert(sym.coordinate(), idx);

            // Also index unqualified name if qualified (e.g. Type::method -> method)
            if let Some(short_name) = sym.name.rsplit("::").next() {
                if short_name != sym.name {
                    name_to_indices
                        .entry(short_name.to_string())
                        .or_default()
                        .push(idx);
                }
            }
        }

        let mut callers_map: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut callees_map: HashMap<usize, Vec<usize>> = HashMap::new();

        // Resolve reference edges
        for (u_idx, sym) in symbols.iter().enumerate() {
            for ref_name in &sym.references {
                if let Some(candidate_indices) = name_to_indices.get(ref_name) {
                    // Match candidates (prefer same file, otherwise all matching)
                    let same_file_match = candidate_indices
                        .iter()
                        .copied()
                        .find(|&v_idx| symbols[v_idx].file == sym.file);

                    if let Some(v_idx) = same_file_match {
                        if u_idx != v_idx {
                            add_edge(u_idx, v_idx, &mut callers_map, &mut callees_map);
                        }
                    } else {
                        for &v_idx in candidate_indices {
                            if u_idx != v_idx {
                                add_edge(u_idx, v_idx, &mut callers_map, &mut callees_map);
                            }
                        }
                    }
                }
            }
        }

        // Compute in-degree PageRank centrality
        pagerank::compute_in_degree_pagerank(&mut symbols, &callers_map, &callees_map, None);

        SymbolGraph {
            symbols,
            callers_map,
            callees_map,
        }
    }

    pub fn compute_personalized_pagerank(&mut self, focus_target: &str) {
        let focus_indices = self.find_symbol_indices(focus_target);
        pagerank::compute_in_degree_pagerank(
            &mut self.symbols,
            &self.callers_map,
            &self.callees_map,
            Some(&focus_indices),
        );
    }

    pub fn find_symbol_indices(&self, target: &str) -> Vec<usize> {
        let mut results = Vec::new();

        // 1. Exact coordinate match: "path/file.rs:symbol"
        if let Some((target_file, target_name)) = target.split_once(':') {
            let tf_name = Path::new(target_file).file_name().and_then(|n| n.to_str()).unwrap_or(target_file);

            for (idx, s) in self.symbols.iter().enumerate() {
                if s.name == target_name || s.name.ends_with(&format!("::{}", target_name)) {
                    let sf_name = Path::new(&s.file).file_name().and_then(|n| n.to_str()).unwrap_or(&s.file);
                    if target_file == s.file
                        || target_file.ends_with(&s.file)
                        || s.file.ends_with(target_file)
                        || tf_name == sf_name
                    {
                        results.push(idx);
                    }
                }
            }
            if !results.is_empty() {
                return results;
            }
        }

        // 2. Name match (exact or qualified suffix)
        for (idx, s) in self.symbols.iter().enumerate() {
            if s.name == target
                || s.name.ends_with(&format!("::{}", target))
                || s.name.ends_with(&format!(".{}", target))
            {
                results.push(idx);
            }
        }

        results
    }

    pub fn callers(&self, target: &str) -> Vec<&Symbol> {
        let indices = self.find_symbol_indices(target);
        let mut caller_symbols = Vec::new();

        for target_idx in indices {
            if let Some(callers) = self.callers_map.get(&target_idx) {
                for &caller_idx in callers {
                    let sym = &self.symbols[caller_idx];
                    if !caller_symbols.iter().any(|s: &&Symbol| s.coordinate() == sym.coordinate()) {
                        caller_symbols.push(sym);
                    }
                }
            }
        }

        caller_symbols
    }

    pub fn callees(&self, target: &str) -> Vec<&Symbol> {
        let indices = self.find_symbol_indices(target);
        let mut callee_symbols = Vec::new();

        for target_idx in indices {
            if let Some(callees) = self.callees_map.get(&target_idx) {
                for &callee_idx in callees {
                    let sym = &self.symbols[callee_idx];
                    if !callee_symbols.iter().any(|s: &&Symbol| s.coordinate() == sym.coordinate()) {
                        callee_symbols.push(sym);
                    }
                }
            }
        }

        callee_symbols
    }

    pub fn build_slice(
        &self,
        target: &str,
        follow_local: bool,
        with_imports: bool,
    ) -> Result<SliceResult> {
        // Line range check
        if target.contains(":#L") {
            return slice_line_coordinate(target, with_imports);
        }

        let indices = self.find_symbol_indices(target);
        if indices.is_empty() {
            bail!("Symbol '{}' not found in workspace.", target);
        }

        if !target.contains(':') && indices.len() > 1 {
            let mut matches: Vec<&Symbol> = indices.iter().map(|&i| &self.symbols[i]).collect();
            matches.sort_by(|a, b| b.centrality.partial_cmp(&a.centrality).unwrap_or(std::cmp::Ordering::Equal));
            let coords: Vec<String> = matches.iter().map(|s| format!("  - `{}` ({}) [rank: {:.4}]", s.coordinate(), s.kind.as_str(), s.centrality)).collect();
            bail!("Ambiguous symbol '{}'. Multiple matches found, please specify full coordinate:\n{}", target, coords.join("\n"));
        }

        let target_idx = indices[0];
        let sym = &self.symbols[target_idx];

        let callers: Vec<String> = self
            .callers(target)
            .into_iter()
            .map(|s| s.coordinate())
            .collect();

        let callee_syms = self.callees(target);
        let callees: Vec<String> = callee_syms
            .iter()
            .map(|s| s.coordinate())
            .collect();

        let mut body = if sym.end_line - sym.start_line > 250 {
            let lines: Vec<&str> = sym.body.lines().collect();
            let head = lines[..200].join("\n");
            let tail = lines[lines.len() - 30..].join("\n");
            format!("{}\n\n// ... [{} lines truncated for token efficiency] ...\n\n{}", head, lines.len() - 230, tail)
        } else {
            sym.body.clone()
        };

        if follow_local {
            let local_callees: Vec<&&Symbol> = callee_syms
                .iter()
                .filter(|c| c.file == sym.file)
                .collect();

            if !local_callees.is_empty() {
                body.push_str("\n\n// --- Inlined Local Callees (--follow-local) ---\n");
                for lc in local_callees {
                    body.push_str(&format!("\n// Symbol: `{}` (L{}-L{})\n{}\n", lc.name, lc.start_line, lc.end_line, lc.body));
                }
            }
        }

        let imports = if with_imports {
            let p = Path::new(&sym.file);
            let imps = extract_file_imports(p);
            if imps.is_empty() {
                None
            } else {
                Some(imps)
            }
        } else {
            None
        };

        Ok(SliceResult {
            coordinate: sym.coordinate(),
            file: sym.file.clone(),
            symbol: Some(sym.clone()),
            line_range: Some((sym.start_line, sym.end_line)),
            content: body,
            callers,
            callees,
            total_lines: sym.end_line - sym.start_line + 1,
            imports,
        })
    }
}

pub fn extract_file_imports(file_path: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(file_path) else {
        return Vec::new();
    };

    let mut imports = Vec::new();
    let mut in_multiline_import = false;
    let mut multiline_buf = String::new();

    for line in content.lines().take(100) {
        let trimmed = line.trim();

        if in_multiline_import {
            multiline_buf.push_str(line);
            multiline_buf.push('\n');
            if trimmed.contains(')') || trimmed.contains('}') || trimmed.ends_with(';') {
                in_multiline_import = false;
                imports.push(multiline_buf.trim_end().to_string());
                multiline_buf.clear();
            }
            continue;
        }

        if trimmed.starts_with("use ")
            || trimmed.starts_with("pub use ")
            || trimmed.starts_with("extern crate ")
        {
            if trimmed.ends_with(';') {
                imports.push(trimmed.to_string());
            } else {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            }
        } else if trimmed.starts_with("import ")
            || trimmed.starts_with("import{")
            || trimmed.starts_with("import type ")
        {
            if trimmed.ends_with(';') || trimmed.ends_with('\'') || trimmed.ends_with('"') {
                imports.push(trimmed.to_string());
            } else {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            }
        } else if trimmed.starts_with("from ") && trimmed.contains(" import ") {
            if trimmed.ends_with('\\') || (trimmed.contains('(') && !trimmed.contains(')')) {
                in_multiline_import = true;
                multiline_buf.push_str(line);
                multiline_buf.push('\n');
            } else {
                imports.push(trimmed.to_string());
            }
        } else if trimmed.starts_with("import ") {
            imports.push(trimmed.to_string());
        } else if trimmed.starts_with("import (") || trimmed == "import (" {
            in_multiline_import = true;
            multiline_buf.push_str(line);
            multiline_buf.push('\n');
        } else if trimmed.starts_with("import \"")
            || ((trimmed.starts_with("const ") || trimmed.starts_with("let "))
                && trimmed.contains("= require("))
        {
            imports.push(trimmed.to_string());
        }
    }

    imports
}

fn add_edge(
    caller_idx: usize,
    callee_idx: usize,
    callers_map: &mut HashMap<usize, Vec<usize>>,
    callees_map: &mut HashMap<usize, Vec<usize>>,
) {
    let callers = callers_map.entry(callee_idx).or_default();
    if !callers.contains(&caller_idx) {
        callers.push(caller_idx);
    }

    let callees = callees_map.entry(caller_idx).or_default();
    if !callees.contains(&callee_idx) {
        callees.push(callee_idx);
    }
}

fn slice_line_coordinate(target: &str, with_imports: bool) -> Result<SliceResult> {
    let idx = target.find(":#L").unwrap();
    let file_str = &target[..idx];
    let range_str = &target[idx + 3..];

    let path = Path::new(file_str);
    if !path.exists() {
        bail!("File not found: {}", file_str);
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", file_str))?;

    let (start, end) = if let Some(dash) = range_str.find('-') {
        let s: usize = range_str[..dash].parse().context("Invalid start line")?;
        let e: usize = range_str[dash + 1..].parse().context("Invalid end line")?;
        (s, e)
    } else {
        let s: usize = range_str.parse().context("Invalid line number")?;
        (s, s)
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Normalize: accept reversed ranges, floor at line 1, fail past end of file.
    let (start, end) = if start > end { (end, start) } else { (start, end) };
    let start = start.max(1);
    if start > total_lines {
        bail!(
            "Line {} is past the end of {} ({} lines).",
            start,
            file_str,
            total_lines
        );
    }

    let start_idx = start - 1;
    let end_idx = end.min(total_lines);

    let mut sliced = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        let line_num = start + i;
        sliced.push_str(&format!("{:4} | {}\n", line_num, line));
    }

    let imports = if with_imports {
        let imps = extract_file_imports(path);
        if imps.is_empty() {
            None
        } else {
            Some(imps)
        }
    } else {
        None
    };

    Ok(SliceResult {
        coordinate: target.to_string(),
        file: file_str.to_string(),
        symbol: None,
        line_range: Some((start, end)),
        content: sliced,
        callers: Vec::new(),
        callees: Vec::new(),
        total_lines: end_idx - start_idx,
        imports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(lines: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 1..=lines {
            writeln!(f, "line {i}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn line_zero_does_not_underflow() {
        // (start - 1) on line 0 wrapped to usize::MAX. Regression: M3/graph.rs:361.
        let f = fixture(10);
        let target = format!("{}:#L0-5", f.path().display());
        let res = slice_line_coordinate(&target, false).unwrap();
        assert_eq!(res.line_range, Some((1, 5)));
        assert!(res.content.contains("line 1"));
    }

    #[test]
    fn reversed_ranges_are_normalized() {
        // lines[7..2] panicked. Regression: M3/graph.rs:365.
        let f = fixture(10);
        let target = format!("{}:#L8-2", f.path().display());
        let res = slice_line_coordinate(&target, false).unwrap();
        assert_eq!(res.line_range, Some((2, 8)));
        assert!(res.content.contains("line 2") && res.content.contains("line 8"));
    }

    #[test]
    fn start_past_end_of_file_is_an_error() {
        let f = fixture(10);
        let target = format!("{}:#L400-500", f.path().display());
        let err = slice_line_coordinate(&target, false).unwrap_err().to_string();
        assert!(err.contains("past the end"), "got: {err}");
    }

    #[test]
    fn end_past_end_of_file_clamps() {
        let f = fixture(10);
        let target = format!("{}:#L8-500", f.path().display());
        let res = slice_line_coordinate(&target, false).unwrap();
        assert!(res.content.contains("line 10"));
    }
}
