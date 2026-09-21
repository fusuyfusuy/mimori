---
scope: "Graph Engine & Analysis"
score: 8.2
status: "MODERATE"
critical_findings: 1
invariant_breaches: []
---

# Scope 2 Audit Report: Graph Engine & Analysis

## Executive Summary
A comprehensive audit was performed on `mimori`'s dependency graph, PageRank centrality, blast radius, and architectural mapping engine across all 6 target files (`src/graph/mod.rs`, `pagerank.rs`, `map.rs`, `blast.rs`, `missing.rs`, and `doctor.rs`).
The architecture demonstrates exceptional hygiene: zero unguarded `.unwrap()` calls in non-test paths, pure mathematical decoupling in PageRank and missing file detection, robust iterative BFS traversal with strict depth caps and cycle resistance, and full determinism.
However, one critical edge-resolution defect was uncovered where recursive self-calls erroneously link across files to foreign symbols with identical names, alongside numerical normalization leaks in personalized PageRank and performance overhead in inner power-iteration loops.

## Evaluation by Dimension

### 1. Correctness (Score: 7.8/10)
- **PageRank Convergence & Stability**: Standard In-Degree PageRank convergence via power iteration with L1 residual threshold $10^{-6}$ and dangling node mass redistribution is mathematically sound. However, personalized PageRank (`pagerank.rs:53-65`) assigns mass $1/k$ without deduplicating or checking out-of-bounds indices, causing total personalization mass $\sum p < 1.0$ and probability decay.
- **Edge Resolution & Disambiguation**: Tiered resolution (same-file $\to$ same-dir $\to$ unique-global) provides strong disambiguation. However, self-filtering (`v != u_idx`) in Tier 1 (`mod.rs:210`) causes self-recursive calls to drop through to Tier 3, where `non_self` filtering links the call to a foreign function with the same name (`mod.rs:291-306`).
- **Blast Closure**: BFS upstream/downstream closure correctly partitions call edges from value-uses. `resolve_upstream_targets` (`mod.rs:532-554`) fails to filter constructors by file, allowing foreign constructors of identically named classes to contaminate the upstream cone.

### 2. Robustness (Score: 8.8/10)
- **Graph Cycles & Disconnected Components**: Cycles in call graphs are handled safely in both PageRank (power iteration with damping factor $d=0.85$) and blast traversal (visited set prevents cyclic re-traversal). Disconnected singletons share dangling mass redistribution.
- **Traversal Limits**: Blast traversal is purely iterative via `VecDeque` with explicit `depth_limit` guards (`blast.rs:191`), preventing call-stack overflows.
- **Doctor Diagnostics**: Limited to single-node isolated components (`doctor.rs:134`). Multi-node dead code islands (dead trees or dead cycles) are completely invisible to doctor.

### 3. Performance (Score: 7.9/10)
- **Adjacency Representation**: Directed adjacency lists `callers_map` and `callees_map` (`HashMap<usize, Vec<usize>>`) provide compact sparse representations.
- **Power Iteration Bottleneck**: In `pagerank.rs:96-101`, `edge_w(u, v)` queries `HashMap<(usize, usize), f64>` inside the nested `MAX_ITERATIONS \times |E|` loop. Transition probabilities are invariant across iterations and should be precomputed into contiguous adjacency weight vectors, eliminating up to millions of hash lookups on large graphs.
- **Index Lookup Efficiency**: Pre-sorting candidate lists by file ID (`mod.rs:142-148`) allows binary search (`partition_point`) for same-file probes, avoiding $O(N^2)$ scaling.

### 4. Clean Architecture & Invariants (Score: 9.0/10)
- **Pure Algorithms**: Core computations in `pagerank.rs`, `missing.rs`, and `blast.rs` operate on pure slices and graph references with zero side effects.
- **Panic Safety**: All production calls use safe pattern matching, `.unwrap_or()`, or explicit bounds checking.
- **Contract Adherence**: Clear separation of call edges versus value mentions; edge-resolution accounting cleanly surfaced to outputs.

---

## Detailed Findings

### CRITICAL-1: Recursive Self-Call Cross-Wiring via Tier 3 Fallthrough
- **Location**: `src/graph/mod.rs:201-225, 291-306`
- **Issue**: In `SymbolGraph::new`, when resolving a call from `u_idx` whose target matches `u_idx`'s own name (self-recursion), line 210 executes `same_file.retain(|&v| v != u_idx)`. This empties `same_file`, causing Tier 1 to bypass the self-call. If another file defines a callable symbol with the same name, `callable` has 2 candidates. In Tier 3, `non_self` filters out `u_idx`, leaving exactly 1 foreign candidate (`non_self.len() == 1`). Line 297 adds a weighted call edge from `u_idx` to the foreign function in another file.
- **Impact**: Recursively defined functions create phantom cross-file edges, inflating centrality and corrupting blast radiuses.

### MEDIUM-1: PageRank Personalization Vector Leaks Probability Mass
- **Location**: `src/graph/pagerank.rs:53-65`
- **Issue**: `mass` is calculated as `1.0 / indices.len() as f64`. If `focus_indices` contains duplicates or indices $\ge n$, `p[idx]` overwrites or skips entries. $\sum_{v} p[v]$ drops below 1.0. Because damping and dangling redistribution multiply by $p[v]$, scores exponentially leak mass towards zero on every power iteration.
- **Impact**: Skewed centrality rankings and delayed convergence during `--focus` queries.

### MEDIUM-2: Cross-File Constructor Contamination in Upstream Blasts
- **Location**: `src/graph/mod.rs:532-554`
- **Issue**: `resolve_upstream_targets` queries `name_to_indices` for `format!("{}{}", sym.name, suffix)` without asserting that the constructor shares the same file or parent path as `sym`.
- **Impact**: For common struct names (e.g. `Config`, `Context`), upstream queries on `fileA:Config` inadvertently pull in caller edges of `fileB:Config::new`.

### MEDIUM-3: In-Loop Hash Table Lookups in PageRank Power Iteration
- **Location**: `src/graph/pagerank.rs:96-101`
- **Issue**: The inner loop repeatedly looks up `edge_weights.get(&(u, v))` for every incoming edge on every iteration step ($100 \times |E|$ hash lookups).
- **Impact**: Significant CPU cache thrashing and execution latency on large repositories (>10k symbols).

### MINOR-1: Dead-Weight Islands Invisible in Doctor
- **Location**: `src/graph/doctor.rs:132-149`
- **Issue**: `Doctor` checks `!has_callers && !has_callees`. Isolated call graphs (e.g. function A calling function B where neither is called elsewhere) have callers/callees and are omitted from `likely_dead`.
- **Impact**: Multi-symbol dead code remains undetected.

### MINOR-2: Overbroad Framework Suffix Exemption
- **Location**: `src/graph/doctor.rs:189`
- **Issue**: `leaf.ends_with("Router")` treats any function ending with "Router" (e.g., `makeRouter`) as an active framework entry point.
- **Impact**: Suppresses legitimate dead code reporting on helper functions.

---

## Remediation Roadmap
1. **Fix Recursive Self-Resolution**: Handle `u_idx` in Tier 1 (`mod.rs:201-225`). If the target is `u_idx`, acknowledge the self-edge or drop cleanly without falling through to Tier 3.
2. **Normalize Personalization Vector**: Deduplicate and validate `focus_indices` in `pagerank.rs`, guaranteeing $\sum p = 1.0$.
3. **Constrain Constructor Folding**: In `resolve_upstream_targets`, filter candidate constructors to those matching `sym.file`.
4. **Precompute Transition Probabilities**: Precalculate normalized outgoing weights $\frac{w(u,v)}{total(u)}$ into a contiguous CSR or adjacency vector before entering the PageRank iteration loop.
