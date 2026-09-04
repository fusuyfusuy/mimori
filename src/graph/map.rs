use crate::graph::SymbolGraph;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A symbol as it appears in a map.
///
/// Deliberately not `Symbol`: that type carries `body`, so `map --json` used to
/// serialize the entire source of the repository -- 2.0x the bytes it indexed.
/// Source is what `slice` is for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapSymbol {
    pub name: String,
    pub kind: String,
    pub coordinate: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub centrality: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMap {
    pub file: String,
    pub symbols: Vec<MapSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapResult {
    pub scope: Option<String>,
    pub focus: Option<String>,
    pub modules: Vec<ModuleMap>,
    pub total_symbols: usize,
    /// Set when `--limit` dropped lower-ranked symbols from the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_from: Option<usize>,
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
        out.push_str(&format!("*Total Symbols*: {}", self.total_symbols));
        if let Some(total) = self.truncated_from {
            out.push_str(&format!(" of {} (--limit)", total));
        }
        out.push_str("\n\n");

        if self.modules.is_empty() {
            out.push_str("No symbols matching the specified scope.\n");
            return out;
        }

        for module in &self.modules {
            out.push_str(&format!("### 📁 `{}`\n", module.file));
            for s in &module.symbols {
                out.push_str(&format!(
                    "- 🔹 **`{}`** ({}) [rank: {:.4}] → L{}-L{}\n",
                    s.name, s.kind, s.centrality, s.start_line, s.end_line
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
    limit: Option<usize>,
) -> MapResult {
    let clean_scope = scope.map(|s| s.trim_start_matches("./"));

    let mut selected: Vec<&crate::model::Symbol> = graph
        .symbols
        .iter()
        .filter(|s| match clean_scope {
            None => true,
            Some(sc) => in_scope(&s.file, sc),
        })
        .collect();

    selected.sort_by(|a, b| {
        b.centrality
            .partial_cmp(&a.centrality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_symbols = selected.len();
    let truncated_from = match limit {
        Some(n) if n < total_symbols => {
            selected.truncate(n);
            Some(total_symbols)
        }
        _ => None,
    };

    let mut file_groups: BTreeMap<String, Vec<MapSymbol>> = BTreeMap::new();
    for s in &selected {
        file_groups
            .entry(s.file.clone())
            .or_default()
            .push(MapSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                coordinate: s.coordinate(),
                start_line: s.start_line,
                end_line: s.end_line,
                signature: s.signature.clone(),
                centrality: s.centrality,
            });
    }

    let mut modules: Vec<ModuleMap> = file_groups
        .into_iter()
        .map(|(file, mut symbols)| {
            symbols.sort_by(|a, b| {
                b.centrality
                    .partial_cmp(&a.centrality)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            ModuleMap { file, symbols }
        })
        .collect();

    // Rank modules by their strongest symbol.
    modules.sort_by(|a, b| {
        let max_a = a.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        let max_b = b.symbols.first().map(|s| s.centrality).unwrap_or(0.0);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    MapResult {
        scope: scope.map(|s| s.to_string()),
        focus: focus.map(|s| s.to_string()),
        modules,
        total_symbols: selected.len(),
        truncated_from,
    }
}

/// Scope matches on path components, so `--scope src` does not also select
/// `foo/src2/`.
fn in_scope(file: &str, scope: &str) -> bool {
    let file = file.trim_start_matches("./");
    let scope = scope.trim_start_matches("./").trim_end_matches('/');
    file == scope
        || file.starts_with(&format!("{scope}/"))
        || file
            .split('/')
            .any(|c| c == scope)
}
