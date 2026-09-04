use crate::graph::SymbolGraph;
use crate::model::Symbol;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMap {
    pub file: String,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapResult {
    pub scope: Option<String>,
    pub focus: Option<String>,
    pub modules: Vec<ModuleMap>,
    pub total_symbols: usize,
}

impl MapResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Repository Map (Ranked by Centrality)\n\n");

        if let Some(scope) = &self.scope {
            out.push_str(&format!("*Scope*: `{}` | ", scope));
        }
        if let Some(focus) = &self.focus {
            out.push_str(&format!("*Focus*: `{}` | ", focus));
        }
        out.push_str(&format!("*Total Symbols*: {}\n\n", self.total_symbols));

        if self.modules.is_empty() {
            out.push_str("No symbols matching the specified scope.\n");
            return out;
        }

        for module in &self.modules {
            out.push_str(&format!("### 📁 `{}`\n", module.file));
            for s in &module.symbols {
                out.push_str(&format!(
                    "- 🔹 **`{}`** ({}) [rank: {:.4}] → L{}-L{}\n",
                    s.name, s.kind.as_str(), s.centrality, s.start_line, s.end_line
                ));
                if !s.signature.is_empty() {
                    out.push_str(&format!("    `{}`\n", s.signature));
                }
            }
            out.push('\n');
        }

        out
    }
}

pub fn generate_map(
    graph: &SymbolGraph,
    scope: Option<&str>,
    focus: Option<&str>,
) -> MapResult {
    let mut file_groups: BTreeMap<String, Vec<Symbol>> = BTreeMap::new();

    let clean_scope = scope.map(|s| s.trim_start_matches("./"));

    for s in &graph.symbols {
        let file_clean = s.file.trim_start_matches("./");

        if let Some(sc) = clean_scope {
            if !file_clean.starts_with(sc) && !s.file.contains(sc) {
                continue;
            }
        }

        file_groups.entry(s.file.clone()).or_default().push(s.clone());
    }

    let mut modules = Vec::new();
    let mut total_symbols = 0;

    for (file, mut syms) in file_groups {
        syms.sort_by(|a, b| b.centrality.partial_cmp(&a.centrality).unwrap_or(std::cmp::Ordering::Equal));
        total_symbols += syms.len();
        modules.push(ModuleMap { file, symbols: syms });
    }

    // Sort modules by the maximum centrality of any symbol within them
    modules.sort_by(|a, b| {
        let max_a = a.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        let max_b = b.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        max_b.partial_cmp(&max_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    MapResult {
        scope: scope.map(|s| s.to_string()),
        focus: focus.map(|s| s.to_string()),
        modules,
        total_symbols,
    }
}
