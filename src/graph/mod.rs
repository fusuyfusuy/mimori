pub mod blast;
pub mod map;
pub mod pagerank;

use crate::model::{Coordinate, SliceResult, Symbol};
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
        let _p = crate::Phase::start("  name index");
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

        drop(_p);
        let _p = crate::Phase::start("  edge resolve");
        let mut callers_map: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut callees_map: HashMap<usize, Vec<usize>> = HashMap::new();

        // Intern file paths so the same-file test is a u32 compare.
        let mut file_ids: HashMap<&str, u32> = HashMap::new();
        let sym_file_id: Vec<u32> = symbols
            .iter()
            .map(|s| {
                let next = file_ids.len() as u32;
                *file_ids.entry(s.file.as_str()).or_insert(next)
            })
            .collect();

        // Order each candidate list by file so the same-file probe is a binary
        // search rather than a scan. Without this, a name defined once per file
        // costs O(files) per reference, which is quadratic in workspace size --
        // and names like `new`, `build` or `Config` are defined in most files.
        let mut by_name: HashMap<&str, Vec<(u32, usize)>> =
            HashMap::with_capacity(name_to_indices.len());
        for (name, indices) in &name_to_indices {
            let mut v: Vec<(u32, usize)> =
                indices.iter().map(|&i| (sym_file_id[i], i)).collect();
            v.sort_unstable();
            by_name.insert(name.as_str(), v);
        }

        for (u_idx, sym) in symbols.iter().enumerate() {
            let u_file = sym_file_id[u_idx];
            for ref_name in &sym.references {
                let Some(candidates) = by_name.get(ref_name.as_str()) else {
                    continue;
                };

                // partition_point, not binary_search: entries are sorted by
                // (file, index), so this finds the *first* symbol in the file,
                // matching the original scan's "first candidate in index order".
                let pos = candidates.partition_point(|&(f, _)| f < u_file);
                let same_file = candidates.get(pos).filter(|&&(f, _)| f == u_file);

                let resolved = match same_file {
                    Some(&(_, v_idx)) => Some(v_idx),
                    None if candidates.len() == 1 => Some(candidates[0].1),
                    None => None,
                };

                if let Some(v_idx) = resolved {
                    if u_idx != v_idx {
                        add_edge(u_idx, v_idx, &mut callers_map, &mut callees_map);
                    }
                }
            }
        }

        drop(_p);
        let _p = crate::Phase::start("  pagerank");
        pagerank::compute_in_degree_pagerank(&mut symbols, &callers_map, &callees_map, None);

        drop(_p);
        SymbolGraph {
            symbols,
            callers_map,
            callees_map,
        }
    }

    /// Bias the ranking toward symbols whose name or file contains `term`.
    ///
    /// `--seed` parsed and was discarded before this existed, while three
    /// documents described it as working.
    pub fn seed_indices(&self, term: &str) -> Vec<usize> {
        let needle = term.to_lowercase();
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.name.to_lowercase().contains(&needle)
                    || s.file.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn apply_personalization(&mut self, indices: &[usize]) {
        pagerank::compute_in_degree_pagerank(
            &mut self.symbols,
            &self.callers_map,
            &self.callees_map,
            Some(indices),
        );
    }

    pub fn compute_personalized_pagerank(&mut self, focus: &Coordinate) {
        let focus_indices = self.resolve_all(focus);
        pagerank::compute_in_degree_pagerank(
            &mut self.symbols,
            &self.callers_map,
            &self.callees_map,
            Some(&focus_indices),
        );
    }

    /// Resolve a coordinate to every symbol in the winning match tier.
    ///
    /// Used by up/down/blast/focus, which legitimately want all matches for a
    /// bare name.
    pub fn resolve_all(&self, coord: &Coordinate) -> Vec<usize> {
        match self.resolve(coord) {
            Resolution::Unique(idx) => vec![idx],
            Resolution::Ambiguous(indices) => indices,
            Resolution::NotFound => Vec::new(),
        }
    }

    /// Resolve a coordinate, distinguishing "one match" from "several".
    ///
    /// Matching is tiered and the first tier that produces candidates is the
    /// answer -- but more than one candidate in a tier is Ambiguous, never a
    /// silent pick. Previously an exact coordinate could match another file by
    /// basename and `build_slice` would take `indices[0]`, returning a
    /// different file's source under the requested path.
    pub fn resolve(&self, coord: &Coordinate) -> Resolution {
        let Some(name) = coord.name() else {
            return Resolution::NotFound;
        };

        let by_name: Vec<usize> = self
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| name_matches(&s.name, name))
            .map(|(idx, _)| idx)
            .collect();

        if by_name.is_empty() {
            return Resolution::NotFound;
        }

        let Some(target_file) = coord.file() else {
            return Resolution::from(by_name);
        };

        // Tier 1: the exact workspace-relative path.
        // Tier 2: a path suffix on a component boundary.
        // Tier 3: the basename alone.
        for tier in [
            |a: &Path, b: &Path| a == b,
            component_suffix_match,
            basename_match,
        ] {
            let hits: Vec<usize> = by_name
                .iter()
                .copied()
                .filter(|&idx| tier(Path::new(&self.symbols[idx].file), target_file))
                .collect();
            if !hits.is_empty() {
                return Resolution::from(hits);
            }
        }

        Resolution::NotFound
    }

    pub fn callers(&self, coord: &Coordinate) -> Vec<&Symbol> {
        let indices = self.resolve_all(coord);
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

    pub fn callees(&self, coord: &Coordinate) -> Vec<&Symbol> {
        let indices = self.resolve_all(coord);
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
        coord: &Coordinate,
        follow_local: bool,
        with_imports: bool,
    ) -> Result<SliceResult> {
        if let Coordinate::Lines { file, start, end } = coord {
            return slice_line_coordinate(file, *start, *end, with_imports);
        }

        let target_idx = match self.resolve(coord) {
            Resolution::Unique(idx) => idx,
            Resolution::NotFound => bail!("Symbol '{}' not found in workspace.", coord),
            Resolution::Ambiguous(indices) => {
                let mut matches: Vec<&Symbol> = indices.iter().map(|&i| &self.symbols[i]).collect();
                matches.sort_by(|a, b| {
                    b.centrality
                        .partial_cmp(&a.centrality)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let coords: Vec<String> = matches
                    .iter()
                    .map(|s| {
                        format!(
                            "  - `{}` ({}) [rank: {:.4}]",
                            s.coordinate(),
                            s.kind.as_str(),
                            s.centrality
                        )
                    })
                    .collect();
                bail!(
                    "Ambiguous symbol '{}'. Multiple matches found, please specify full coordinate:\n{}",
                    coord,
                    coords.join("\n")
                );
            }
        };

        let sym = &self.symbols[target_idx];

        let callers: Vec<String> = self
            .callers(coord)
            .into_iter()
            .map(|s| s.coordinate())
            .collect();

        let callee_syms = self.callees(coord);
        let callees: Vec<String> = callee_syms.iter().map(|s| s.coordinate()).collect();

        let mut body = truncate_body(&sym.body, sym.end_line - sym.start_line);

        if follow_local {
            let local_callees: Vec<&&Symbol> =
                callee_syms.iter().filter(|c| c.file == sym.file).collect();

            if !local_callees.is_empty() {
                body.push_str("\n\n// --- Inlined Local Callees (--follow-local) ---\n");
                for lc in local_callees {
                    body.push_str(&format!(
                        "\n// Symbol: `{}` (L{}-L{})\n{}\n",
                        lc.name, lc.start_line, lc.end_line, lc.body
                    ));
                }
            }
        }

        let imports = if with_imports {
            let imps = extract_file_imports(Path::new(&sym.file));
            (!imps.is_empty()).then_some(imps)
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

/// How a coordinate resolved against the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Unique(usize),
    Ambiguous(Vec<usize>),
    NotFound,
}

impl From<Vec<usize>> for Resolution {
    fn from(mut hits: Vec<usize>) -> Self {
        match hits.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Unique(hits.remove(0)),
            _ => Resolution::Ambiguous(hits),
        }
    }
}

fn name_matches(symbol_name: &str, target: &str) -> bool {
    symbol_name == target
        || symbol_name.ends_with(&format!("::{}", target))
        || symbol_name.ends_with(&format!(".{}", target))
}

/// True when one path is a suffix of the other on a component boundary.
fn component_suffix_match(a: &Path, b: &Path) -> bool {
    let av: Vec<_> = a.components().collect();
    let bv: Vec<_> = b.components().collect();
    let n = av.len().min(bv.len());
    n > 0 && av[av.len() - n..] == bv[bv.len() - n..]
}

fn basename_match(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Cap very large bodies, keeping the head and tail.
fn truncate_body(body: &str, span: usize) -> String {
    const LIMIT: usize = 250;
    const HEAD: usize = 200;
    const TAIL: usize = 30;

    let lines: Vec<&str> = body.lines().collect();
    if span <= LIMIT || lines.len() <= HEAD + TAIL {
        return body.to_string();
    }

    format!(
        "{}\n\n// ... [{} lines truncated for token efficiency] ...\n\n{}",
        lines[..HEAD].join("\n"),
        lines.len() - HEAD - TAIL,
        lines[lines.len() - TAIL..].join("\n")
    )
}

pub fn extract_file_imports(file_path: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(file_path) else {
        return Vec::new();
    };

    let mut imports = Vec::new();
    let mut in_multiline_import = false;
    let mut multiline_buf = String::new();

    // Scan the header generously: a 100-line cap silently dropped imports in
    // files with long licence blocks or large import lists.
    for line in content.lines().take(400) {
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
        } else if (trimmed.starts_with("const ") || trimmed.starts_with("let "))
            && trimmed.contains("= require(")
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

/// Slice a line range straight off disk. Needs no index.
pub fn slice_line_coordinate(
    file: &Path,
    start: usize,
    end: usize,
    with_imports: bool,
) -> Result<SliceResult> {
    if !file.exists() {
        bail!("File not found: {}", file.display());
    }

    let content = fs::read_to_string(file)
        .with_context(|| format!("Failed to read file: {}", file.display()))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Normalize: accept reversed ranges, floor at line 1, fail past end of file.
    let (start, end) = if start > end { (end, start) } else { (start, end) };
    let start = start.max(1);
    if start > total_lines {
        bail!(
            "Line {} is past the end of {} ({} lines).",
            start,
            file.display(),
            total_lines
        );
    }

    let start_idx = start - 1;
    let end_idx = end.min(total_lines);

    let mut sliced = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        sliced.push_str(&format!("{:4} | {}\n", start + i, line));
    }

    let imports = if with_imports {
        let imps = extract_file_imports(file);
        (!imps.is_empty()).then_some(imps)
    } else {
        None
    };

    Ok(SliceResult {
        coordinate: format!("{}:#L{}-{}", file.display(), start, end),
        file: file.display().to_string(),
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
    use crate::model::SymbolKind;
    use std::io::Write;

    fn sym(file: &str, name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 1,
            signature: String::new(),
            body: format!("fn {name}() {{ /* {file} */ }}"),
                centrality: 0.0,
            references: vec![],
        }
    }

    fn graph_of(files: &[(&str, &str)]) -> SymbolGraph {
        SymbolGraph::new(files.iter().map(|(f, n)| sym(f, n)).collect())
    }

    fn at(g: &SymbolGraph, raw: &str) -> Resolution {
        g.resolve(&Coordinate::parse(raw).unwrap())
    }

    #[test]
    fn exact_path_beats_a_basename_collision() {
        // Regression M1: `alpha/mod.rs:handler` returned beta's body, because
        // basename equality was one of four equal-weight OR'd conditions and
        // build_slice then took indices[0].
        let g = graph_of(&[("src/alpha/mod.rs", "handler"), ("src/beta/mod.rs", "handler")]);

        let Resolution::Unique(i) = at(&g, "src/alpha/mod.rs:handler") else {
            panic!("exact path must resolve uniquely");
        };
        assert_eq!(g.symbols[i].file, "src/alpha/mod.rs");

        let Resolution::Unique(j) = at(&g, "src/beta/mod.rs:handler") else {
            panic!("exact path must resolve uniquely");
        };
        assert_eq!(g.symbols[j].file, "src/beta/mod.rs");
    }

    #[test]
    fn a_basename_collision_is_ambiguous_not_a_guess() {
        let g = graph_of(&[("src/alpha/mod.rs", "handler"), ("src/beta/mod.rs", "handler")]);
        assert!(matches!(at(&g, "mod.rs:handler"), Resolution::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn a_unique_basename_still_resolves() {
        let g = graph_of(&[("src/auth_service.rs", "login"), ("src/other.rs", "logout")]);
        assert!(matches!(at(&g, "auth_service.rs:login"), Resolution::Unique(_)));
    }

    #[test]
    fn a_path_suffix_resolves_on_component_boundaries() {
        let g = graph_of(&[("src/alpha/mod.rs", "handler"), ("src/beta/mod.rs", "handler")]);
        assert!(matches!(at(&g, "alpha/mod.rs:handler"), Resolution::Unique(_)));

        // "ha/mod.rs" is a string suffix of "alpha/mod.rs" but not a component
        // suffix, so the suffix tier must not resolve it. It falls through to
        // the basename tier, which sees both files and reports ambiguity rather
        // than guessing.
        assert!(matches!(at(&g, "ha/mod.rs:handler"), Resolution::Ambiguous(_)));
    }

    #[test]
    fn component_suffix_ignores_mid_component_string_suffixes() {
        assert!(component_suffix_match(
            Path::new("src/alpha/mod.rs"),
            Path::new("alpha/mod.rs")
        ));
        assert!(!component_suffix_match(
            Path::new("src/alpha/mod.rs"),
            Path::new("ha/mod.rs")
        ));
        assert!(component_suffix_match(
            Path::new("mod.rs"),
            Path::new("src/alpha/mod.rs")
        ));
    }

    #[test]
    fn qualified_bare_names_resolve_through_the_name_tier() {
        // Regression P17: "Store::save" parsed as file "Store", name ":save".
        let g = graph_of(&[("src/lib.rs", "Store::save"), ("src/lib.rs", "other")]);
        assert!(matches!(at(&g, "Store::save"), Resolution::Unique(_)));
        assert!(matches!(at(&g, "save"), Resolution::Unique(_)));
    }

    fn sym_with_refs(file: &str, name: &str, refs: &[&str]) -> Symbol {
        let mut s = sym(file, name);
        s.references = refs.iter().map(|r| r.to_string()).collect();
        s
    }

    #[test]
    fn a_reference_prefers_a_match_in_its_own_file() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["target"]),
            sym("a.rs", "target"),
            sym("b.rs", "target"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].file, "a.rs");
    }

    #[test]
    fn a_unique_name_links_across_files() {
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["only_one"]),
            sym("b.rs", "only_one"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert_eq!(callees.len(), 1, "a unique cross-file name must still link");
    }

    #[test]
    fn an_ambiguous_name_links_to_nothing_rather_than_everything() {
        // Regression M7: one call to `new` used to wire the caller to every
        // `new` in the workspace, inflating centrality and blast radius.
        let g = SymbolGraph::new(vec![
            sym_with_refs("a.rs", "caller", &["new"]),
            sym("b.rs", "new"),
            sym("c.rs", "new"),
            sym("d.rs", "new"),
        ]);
        let callees = g.callees(&Coordinate::parse("a.rs:caller").unwrap());
        assert!(callees.is_empty(), "got {} spurious edges", callees.len());
    }

    #[test]
    fn an_unknown_symbol_is_not_found() {
        let g = graph_of(&[("src/lib.rs", "handler")]);
        assert!(matches!(at(&g, "nope"), Resolution::NotFound));
        assert!(matches!(at(&g, "src/lib.rs:nope"), Resolution::NotFound));
    }

    #[test]
    fn build_slice_refuses_to_guess_between_ambiguous_matches() {
        let g = graph_of(&[("src/alpha/mod.rs", "handler"), ("src/beta/mod.rs", "handler")]);
        let err = g
            .build_slice(&Coordinate::parse("mod.rs:handler").unwrap(), false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Ambiguous"), "got: {err}");
        assert!(err.contains("alpha") && err.contains("beta"), "got: {err}");
    }

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
        let res = slice_line_coordinate(f.path(), 0, 5, false).unwrap();
        assert_eq!(res.line_range, Some((1, 5)));
        assert!(res.content.contains("line 1"));
    }

    #[test]
    fn reversed_ranges_are_normalized() {
        // lines[7..2] panicked. Regression: M3/graph.rs:365.
        let f = fixture(10);
        let res = slice_line_coordinate(f.path(), 8, 2, false).unwrap();
        assert_eq!(res.line_range, Some((2, 8)));
        assert!(res.content.contains("line 2") && res.content.contains("line 8"));
    }

    #[test]
    fn start_past_end_of_file_is_an_error() {
        let f = fixture(10);
        let err = slice_line_coordinate(f.path(), 400, 500, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("past the end"), "got: {err}");
    }

    #[test]
    fn end_past_end_of_file_clamps() {
        let f = fixture(10);
        let res = slice_line_coordinate(f.path(), 8, 500, false).unwrap();
        assert!(res.content.contains("line 10"));
    }
}
