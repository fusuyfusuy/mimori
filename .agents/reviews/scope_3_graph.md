---
scope: "Graph Analytics & Intelligence Engine"
score: 8.8
status: "MINOR"
critical_findings: 0
invariant_breaches: []
---

# Scope 3: Graph Analytics & Intelligence Engine Audit

## Overview
Evaluated graph representation, PageRank centrality, blast radius analysis, doctor diagnostics, and absence sweep: [`src/graph/mod.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/mod.rs), [`src/graph/blast.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/blast.rs), [`src/graph/doctor.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/doctor.rs), [`src/graph/map.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/map.rs), [`src/graph/missing.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/missing.rs), and [`src/graph/pagerank.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/pagerank.rs).

## Findings

### 1. High Complexity Hub: `SymbolGraph::new` (Minor)
- **Location**: [`src/graph/mod.rs#L78-L356`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/mod.rs#L78-L356)
- **Detail**: `SymbolGraph::new` spans 278 lines and bundles name index creation, path interning, tiered edge resolution (same-file, member-call dropping, same-directory, unique-global), external import gates, test-suite penalties, and PageRank invocation. While correctly ordered and heavily tested, the high cyclomatic complexity and nesting depth (>5) make future tier extensions error-prone.

### 2. Numerical Stability & Dangling Mass in Weighted PageRank (Exemplary)
- **Location**: [`src/graph/pagerank.rs#L85-L135`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/pagerank.rs#L85-L135)
- **Detail**: Precomputes normalized caller transition probabilities `P(u -> v) = w(u,v) / out_weight[u]`, avoids inner-loop allocations, properly conserves dangling probability mass via `dangling_sum * personalization[v]`, and validates L1 residual convergence against `1e-6` with a 100-iteration ceiling.

### 3. Traversal Safety: BFS Cycle Isolation & Sink Capping (Exemplary)
- **Location**: [`src/graph/blast.rs#L202-L235`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/blast.rs#L202-L235), [`src/graph/blast.rs#L376-L391`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/blast.rs#L376-L391)
- **Detail**: Graph traversals maintain strict `visited: HashSet<usize>` to prevent cycle loops. Literal sink sweeping enforces `const CAP: usize = 100;` to prevent token bloat during transitive impact analysis.

## Recommendations
1. Decompose `SymbolGraph::new` into modular stages (`index_names`, `resolve_tier1_same_file`, `resolve_tier2_same_dir`, `resolve_tier3_global`) to improve maintainability and decouple resolution heuristics.
