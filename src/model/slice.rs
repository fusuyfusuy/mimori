use super::symbol::Symbol;
use serde::{Deserialize, Serialize};

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
}

impl SliceResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("### Slice: `{}`\n\n", self.coordinate));
        out.push_str(&format!("- **File**: `{}`\n", self.file));

        if let Some(sym) = &self.symbol {
            out.push_str(&format!("- **Symbol**: `{}` ({})\n", sym.name, sym.kind.as_str()));
            out.push_str(&format!("- **Lines**: L{}-L{}\n", sym.start_line, sym.end_line));
            if !sym.signature.is_empty() {
                out.push_str(&format!("- **Signature**: `{}`\n", sym.signature));
            }
        } else if let Some((start, end)) = self.line_range {
            out.push_str(&format!("- **Lines**: L{}-L{}\n", start, end));
        }

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

        if !self.callers.is_empty() {
            out.push_str("- **1-Hop Callers**:\n");
            for c in &self.callers {
                out.push_str(&format!("  - `{}`\n", c));
            }
        }

        if !self.callees.is_empty() {
            out.push_str("- **1-Hop Callees**:\n");
            for c in &self.callees {
                out.push_str(&format!("  - `{}`\n", c));
            }
        }

        out.push_str("\n```\n");
        out.push_str(&self.content);
        if !self.content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");

        out
    }
}
