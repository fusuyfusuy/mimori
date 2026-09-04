use crate::graph::SymbolGraph;
use crate::model::Coordinate;
use crate::model::Symbol;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastNode {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub depth: usize,
    pub is_entry_point: bool,
    pub is_test: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastResult {
    pub target: String,
    pub depth_limit: usize,
    pub affected: Vec<BlastNode>,
    pub entry_points: Vec<String>,
    pub test_suites: Vec<String>,
}

impl BlastResult {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Blast Radius: `{}` (Depth: {}, Affected: {})\n\n",
            self.target,
            self.depth_limit,
            self.affected.len()
        ));

        if self.affected.is_empty() {
            out.push_str("No upstream callers affected. This symbol is an isolated root or leaf.\n");
            return out;
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

        out.push_str("### 🌊 Transitive Call Tree\n");
        for node in &self.affected {
            let indent = "  ".repeat(node.depth);
            let tag = if node.is_entry_point {
                " [Entry Point]"
            } else if node.is_test {
                " [Test]"
            } else {
                ""
            };
            out.push_str(&format!(
                "{}- (d={}) **`{}`** ({}) → `{}`{}\n",
                indent, node.depth, node.name, node.kind, node.coordinate, tag
            ));
        }

        out
    }
}

pub fn calculate_blast_radius(
    graph: &SymbolGraph,
    coord: &Coordinate,
    depth_limit: usize,
) -> Result<BlastResult> {
    let indices = graph.resolve_all(coord);
    if indices.is_empty() {
        bail!("Target symbol '{}' not found in workspace.", coord);
    }

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    for &idx in &indices {
        visited.insert(idx);
        queue.push_back((idx, 0));
    }

    let mut affected = Vec::new();
    let mut entry_points = Vec::new();
    let mut test_suites = Vec::new();

    while let Some((curr_idx, curr_depth)) = queue.pop_front() {
        if curr_depth >= depth_limit {
            continue;
        }

        if let Some(callers) = graph.callers_map.get(&curr_idx) {
            for &caller_idx in callers {
                if !visited.contains(&caller_idx) {
                    visited.insert(caller_idx);
                    let sym = &graph.symbols[caller_idx];
                    let next_depth = curr_depth + 1;

                    let is_entry = check_is_entry_point(sym);
                    let is_test = check_is_test(sym);

                    if is_entry && !entry_points.contains(&sym.coordinate()) {
                        entry_points.push(sym.coordinate());
                    }
                    if is_test && !test_suites.contains(&sym.file) {
                        test_suites.push(sym.file.clone());
                    }

                    affected.push(BlastNode {
                        name: sym.name.clone(),
                        kind: sym.kind.as_str().to_string(),
                        file: sym.file.clone(),
                        coordinate: sym.coordinate(),
                        depth: next_depth,
                        is_entry_point: is_entry,
                        is_test,
                    });

                    queue.push_back((caller_idx, next_depth));
                }
            }
        }
    }

    Ok(BlastResult {
        target: coord.to_string(),
        depth_limit,
        affected,
        entry_points,
        test_suites,
    })
}

fn check_is_entry_point(sym: &Symbol) -> bool {
    sym.name == "main"
        || sym.name.ends_with("::main")
        || sym.name.starts_with("post_")
        || sym.name.starts_with("get_")
        || sym.name.starts_with("handle_")
        || sym.file.contains("main.")
        || sym.file.contains("index.")
        || sym.file.contains("app.")
}

fn check_is_test(sym: &Symbol) -> bool {
    sym.name.starts_with("test_")
        || sym.name.contains("test")
        || sym.file.contains("test")
        || sym.file.contains("spec")
}
