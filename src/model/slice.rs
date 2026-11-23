use super::symbol::Symbol;
use serde::{Deserialize, Serialize};

/// Typed payload for an inlined `--follow-local` callee, avoiding in-band
/// string sentinel collisions when slicing source code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlinedCallee {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceResult {
    pub coordinate: String,
    pub file: String,
    pub symbol: Option<Symbol>,
    pub line_range: Option<(usize, usize)>,
    pub content: String,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
    pub total_lines: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_callees: Vec<InlinedCallee>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_notes: Vec<String>,
}

/// Rough token estimate, calibrated per language instead of one constant.
fn chars_per_token_for(file: &str) -> usize {
    match file.rsplit('.').next().unwrap_or("") {
        "rs" | "go" => 3,
        "py" => 5,
        _ => 4,
    }
}

impl SliceResult {
    fn estimate_tokens(&self, s: &str) -> usize {
        s.chars().count().div_ceil(chars_per_token_for(&self.file))
    }

    pub fn to_markdown(&self) -> String {
        self.render_markdown(false)
    }

    pub fn to_markdown_numbered(&self) -> String {
        self.render_markdown(true)
    }

    pub fn render_markdown(&self, numbered: bool) -> String {
        format!(
            "{}{}{}{}{}{}",
            self.header_markdown(),
            self.memory_notes_markdown(),
            self.imports_markdown(),
            self.callers_markdown(),
            self.callees_markdown(),
            self.format_body(true, numbered)
        )
    }

    pub fn to_markdown_budgeted(&self, budget_tokens: usize) -> String {
        self.render_markdown_budgeted(budget_tokens, false)
    }

    /// Render markdown packed strictly within `budget_tokens` whenever core
    /// (header + body) fits. Drops lowest-priority sections first:
    /// inlined locals, callees, callers, imports, memory notes.
    pub fn render_markdown_budgeted(&self, budget_tokens: usize, numbered: bool) -> String {
        let full = self.render_markdown(numbered);
        if self.estimate_tokens(&full) <= budget_tokens {
            return full;
        }

        let header = self.header_markdown();
        let core_body = self.format_body(false, numbered);
        let core_combined = format!("{header}{core_body}");
        let core_tokens = self.estimate_tokens(&core_combined);

        let locals_rendered = self.format_locals_block(numbered);
        let optional: Vec<(&str, String)> = vec![
            ("inlined local callees", locals_rendered),
            ("callees", self.callees_markdown()),
            ("callers", self.callers_markdown()),
            ("imports", self.imports_markdown()),
            ("memory notes", self.memory_notes_markdown()),
        ]
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .collect();

        let mut keep = vec![true; optional.len()];
        let mut dropped: Vec<&str> = Vec::new();

        let assemble = |keep: &[bool]| -> String {
            let kept = |name: &str| {
                optional
                    .iter()
                    .position(|(n, _)| *n == name)
                    .map(|i| keep[i])
                    .unwrap_or(false)
            };
            let text_of = |name: &str| {
                optional
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("")
            };
            let mut out = header.clone();
            if kept("memory notes") {
                out.push_str(text_of("memory notes"));
            }
            if kept("imports") {
                out.push_str(text_of("imports"));
            }
            if kept("callers") {
                out.push_str(text_of("callers"));
            }
            if kept("callees") {
                out.push_str(text_of("callees"));
            }
            out.push_str(&self.format_body(kept("inlined local callees"), numbered));
            out
        };

        for (i, (name, _)) in optional.iter().enumerate() {
            keep[i] = false;
            dropped.push(name);
            let candidate = assemble(&keep);
            let draft_notice = format!(
                "\n_Truncated to fit --budget {budget_tokens} (~{} tokens): dropped {}._\n",
                self.estimate_tokens(&candidate),
                dropped.join(", ")
            );
            if self.estimate_tokens(&candidate) + self.estimate_tokens(&draft_notice)
                <= budget_tokens
            {
                let mut out = candidate;
                let final_tok = self.estimate_tokens(&out);
                out.push_str(&format!(
                    "\n_Truncated to fit --budget {budget_tokens} (~{final_tok} tokens): dropped {}._\n",
                    dropped.join(", ")
                ));
                return out;
            }
        }

        let mut out = assemble(&keep);
        let final_tok = self.estimate_tokens(&out);
        if core_tokens > budget_tokens || final_tok > budget_tokens {
            out.push_str(&format!(
                "\n_Core slice alone (~{final_tok} tokens) exceeds --budget {budget_tokens}; context omitted._\n"
            ));
            if !dropped.is_empty() {
                out.push_str(&format!("_Dropped: {}._\n", dropped.join(", ")));
            }
        } else if !dropped.is_empty() {
            out.push_str(&format!(
                "\n_Truncated to fit --budget {budget_tokens} (~{final_tok} tokens): dropped {}._\n",
                dropped.join(", ")
            ));
        }
        out
    }

    fn header_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("### Slice: `{}`\n\n", self.coordinate));
        out.push_str(&format!("- **File**: `{}`\n", self.file));

        if let Some(sym) = &self.symbol {
            out.push_str(&format!(
                "- **Symbol**: `{}` ({})\n",
                sym.name,
                sym.kind.as_str()
            ));
            out.push_str(&format!(
                "- **Lines**: L{}-L{}\n",
                sym.start_line, sym.end_line
            ));
            if !sym.signature.is_empty() {
                out.push_str(&format!("- **Signature**: `{}`\n", sym.signature));
            }
        } else if let Some((start, end)) = self.line_range {
            out.push_str(&format!("- **Lines**: L{}-L{}\n", start, end));
        }
        out
    }

    fn memory_notes_markdown(&self) -> String {
        if self.memory_notes.is_empty() {
            return String::new();
        }
        let mut out = String::from("- **Scoped Memory**:\n");
        for note in &self.memory_notes {
            out.push_str(&format!("  - {}\n", note));
        }
        out
    }

    fn imports_markdown(&self) -> String {
        let mut out = String::new();
        if let Some(imports) = &self.imports {
            if !imports.is_empty() {
                out.push_str("- **Backing Imports**:\n```\n");
                for imp in imports {
                    out.push_str(imp);
                    out.push('\n');
                }
                out.push_str("```\n");
            }
        }
        out
    }

    fn callers_markdown(&self) -> String {
        let mut out = String::new();
        if !self.callers.is_empty() {
            out.push_str("- **1-Hop Callers**:\n");
            let limit = 5;
            for c in self.callers.iter().take(limit) {
                out.push_str(&format!("  - `{}`\n", c));
            }
            if self.callers.len() > limit {
                out.push_str(&format!(
                    "  - _5 of {} callers shown (use mimori up for full list)_\n",
                    self.callers.len()
                ));
            }
        }
        out
    }

    fn callees_markdown(&self) -> String {
        let mut out = String::new();
        if !self.callees.is_empty() {
            out.push_str("- **1-Hop Callees**:\n");
            for c in &self.callees {
                out.push_str(&format!("  - `{}`\n", c));
            }
        }
        out
    }

    fn format_locals_block(&self, numbered: bool) -> String {
        if self.local_callees.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\n// --- Inlined Local Callees (--follow-local) ---\n");
        for lc in &self.local_callees {
            out.push_str(&format!(
                "\n// Symbol: `{}` (L{}-L{})\n",
                lc.name, lc.start_line, lc.end_line
            ));
            if numbered {
                let mut lno = lc.start_line.max(1);
                for line in lc.content.lines() {
                    if let Some(skipped) = parse_truncated_line_count(line) {
                        out.push_str(line);
                        out.push('\n');
                        lno += skipped;
                        continue;
                    }
                    if line.is_empty() {
                        out.push_str(&format!("L{}:\n", lno));
                    } else {
                        out.push_str(&format!("L{}: {}\n", lno, line));
                    }
                    lno += 1;
                }
            } else {
                out.push_str(&lc.content);
                if !lc.content.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        out
    }

    fn format_body(&self, include_locals: bool, numbered: bool) -> String {
        let mut out = String::from("\n```\n");
        if !numbered {
            out.push_str(&self.content);
            if !self.content.ends_with('\n') {
                out.push('\n');
            }
        } else {
            self.write_numbered_main(&mut out);
        }
        if include_locals && !self.local_callees.is_empty() {
            out.push_str(&self.format_locals_block(numbered));
        }
        out.push_str("```\n");
        out
    }

    fn write_numbered_main(&self, out: &mut String) {
        let start_line = self
            .symbol
            .as_ref()
            .map(|s| s.start_line)
            .or_else(|| self.line_range.map(|r| r.0))
            .unwrap_or(1)
            .max(1);

        let mut cur_line = start_line;
        let mut lines_iter = self.content.lines().peekable();
        while let Some(line) = lines_iter.next() {
            if line.is_empty() {
                if let Some(&next_line) = lines_iter.peek() {
                    if let Some(skipped) = parse_truncated_line_count(next_line) {
                        if let Some(marker) = lines_iter.next() {
                            if matches!(lines_iter.peek(), Some(&"")) {
                                lines_iter.next();
                            }
                            out.push('\n');
                            out.push_str(marker);
                            out.push_str("\n\n");
                            cur_line += skipped;
                            continue;
                        }
                    }
                }
            }
            if let Some(skipped) = parse_truncated_line_count(line) {
                if matches!(lines_iter.peek(), Some(&"")) {
                    lines_iter.next();
                }
                out.push_str(line);
                out.push('\n');
                cur_line += skipped;
                continue;
            }

            let (lineno, content) = if self.symbol.is_none() {
                if let Some((num_part, code_part)) = line.split_once(" | ") {
                    if let Ok(parsed_num) = num_part.trim().parse::<usize>() {
                        (parsed_num, code_part)
                    } else {
                        (cur_line, line)
                    }
                } else {
                    (cur_line, line)
                }
            } else {
                (cur_line, line)
            };
            out.push_str(&format!("L{}: {}\n", lineno, content));
            cur_line += 1;
        }
    }
}

fn parse_truncated_line_count(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("// ... [")?;
    let num_str = rest.strip_suffix(" lines truncated for token efficiency] ...")?;
    num_str.trim().parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use std::collections::BTreeMap;

    fn fixture() -> SliceResult {
        SliceResult {
            coordinate: "src/a.rs:foo".to_string(),
            file: "src/a.rs".to_string(),
            symbol: Some(Symbol {
                name: "foo".into(),
                kind: SymbolKind::Function,
                file: "src/a.rs".into(),
                start_line: 1,
                end_line: 3,
                signature: "pub fn foo()".into(),
                body: "pub fn foo() {\n  bar();\n}".into(),
                centrality: 0.0,
                calls: vec!["bar".into()],
                mentions: vec![],
                call_counts: BTreeMap::from([("bar".to_string(), 1)]),
                member_calls: vec![],
                external_imports: crate::model::symbol::empty_external_imports(),
                flags: 0,
            }),
            line_range: Some((1, 3)),
            content: "pub fn foo() {\n  bar();\n}".to_string(),
            callers: vec!["src/b.rs:caller".to_string()],
            callees: vec!["src/a.rs:bar".to_string()],
            total_lines: 3,
            imports: Some(vec!["use crate::bar;".to_string()]),
            local_callees: vec![InlinedCallee {
                name: "bar".to_string(),
                start_line: 10,
                end_line: 12,
                content: "fn bar() {}".to_string(),
            }],
            memory_notes: vec![],
        }
    }

    #[test]
    fn unbudgeted_render_keeps_every_section_in_order() {
        let md = fixture().to_markdown();
        assert!(md.contains("### Slice: `src/a.rs:foo`"));
        assert!(md.contains("- **Backing Imports**:"));
        assert!(md.contains("- **1-Hop Callers**:"));
        assert!(md.contains("- **1-Hop Callees**:"));
        assert!(md.contains("Inlined Local Callees"));
        let imports = md.find("Backing Imports").unwrap();
        let callers = md.find("1-Hop Callers").unwrap();
        let callees = md.find("1-Hop Callees").unwrap();
        let body = md.find("  bar();").unwrap();
        assert!(imports < callers && callers < callees && callees < body);
    }

    #[test]
    fn generous_budget_is_byte_identical_to_unbudgeted() {
        let s = fixture();
        assert_eq!(s.to_markdown_budgeted(100_000), s.to_markdown());
    }

    #[test]
    fn tight_budget_keeps_core_and_fits_strictly() {
        let s = fixture();
        let full = s.to_markdown();
        let budget = s.estimate_tokens(&full) - 1;
        let md = s.to_markdown_budgeted(budget);
        assert!(md.contains("### Slice: `src/a.rs:foo`"), "core lost: {md}");
        assert!(md.contains("pub fn foo()"), "body lost: {md}");
        assert!(
            md.contains(&format!("Truncated to fit --budget {budget}")),
            "notice missing: {md}"
        );
        assert!(!md.contains("Inlined Local Callees"), "locals kept: {md}");
        assert!(
            s.estimate_tokens(&md) <= budget,
            "exceeded budget {budget}: got {} in:\n{md}",
            s.estimate_tokens(&md)
        );
    }

    #[test]
    fn zero_budget_keeps_core_and_reports_it_exceeds() {
        let s = fixture();
        let md = s.to_markdown_budgeted(0);
        assert!(md.contains("### Slice: `src/a.rs:foo`"), "core lost: {md}");
        assert!(md.contains("exceeds --budget 0"), "notice missing: {md}");
        assert!(!md.contains("1-Hop Callers"), "context kept: {md}");
    }

    #[test]
    fn caller_truncation_limits_to_five_and_shows_summary() {
        let mut s = fixture();
        s.callers = (1..=7).map(|i| format!("src/a.rs:c{i}")).collect();
        let md = s.to_markdown();
        assert!(md.contains("  - `src/a.rs:c1`"));
        assert!(md.contains("  - `src/a.rs:c5`"));
        assert!(!md.contains("  - `src/a.rs:c6`"));
        assert!(md.contains("  - _5 of 7 callers shown (use mimori up for full list)_"));

        s.callers.truncate(5);
        let md5 = s.to_markdown();
        assert!(md5.contains("  - `src/a.rs:c5`"));
        assert!(!md5.contains("callers shown"));
    }

    #[test]
    fn numbered_render_prefixes_lines_with_linenos_without_sentinel_corruption() {
        let mut s = fixture();
        // Put a fake `// Symbol:` comment inside the local callee to prove no renumbering corruption.
        s.local_callees[0].content = "// Symbol: `fake` (L999-L1005)\nfn bar() {}".to_string();
        let md = s.to_markdown_numbered();
        assert!(md.contains("L1: pub fn foo() {"));
        assert!(md.contains("L10: // Symbol: `fake` (L999-L1005)"));
        assert!(md.contains("L11: fn bar() {}"));
        assert!(!md.contains("L999:"));
    }

    #[test]
    fn numbered_render_with_line_range_respects_start_offset() {
        let mut s = fixture();
        s.symbol = None;
        s.local_callees.clear();
        s.line_range = Some((42, 44));
        s.content = "line a\nline b\nline c\n".into();
        let md = s.to_markdown_numbered();
        assert!(md.contains("L42: line a"));
        assert!(md.contains("L43: line b"));
        assert!(md.contains("L44: line c"));
    }

    #[test]
    fn numbered_render_preserves_pipe_characters_in_symbol_slice() {
        let mut s = fixture();
        s.local_callees.clear();
        s.content = "match x {\n    1 | 2 => true,\n    _ => false,\n}\n".into();
        let md = s.to_markdown_numbered();
        assert!(md.contains("L1: match x {"));
        assert!(md.contains("L2:     1 | 2 => true,"));
    }
}
