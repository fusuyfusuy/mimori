---
scope: "graph-engine"
score: 7.8
status: "MODERATE"
critical_findings: 0
invariant_breaches: []
---
# Graph Engine (src/graph) — sequential pass 2026-10-12

Mode: single-agent fallback (swarm 429). Evidence = live CLI probes; no full source read of graph/mod.rs.

## G1 (Moderate) Call resolution drops most edges -> blast radius under-reports
- `mimori doctor --json` on this repo: 775 resolved vs 407 ambiguous-dropped vs 3651 unresolved.
- Counterexample: `SliceResult::callers_markdown` is called at src/model/slice.rs:52 and :83 via `self.callers_markdown()`;
  `mimori up src/model/slice.rs:SliceResult::callers_markdown` -> "0 callers". Same for `Coordinate::name` (called at src/main.rs:838 as `coord.name()`).
- Effect: `doctor` lists live methods as "likely dead"; `blast`/BLAST_GATE yields false negatives (blast = call edges only).
- Remediation: resolve `self.<m>()` against enclosing impl type; resolve receiver-typed calls when the receiver is a typed local/param.

## G2 (Minor) 407 ambiguous edges dropped silently
- Ambiguity is counted in stats but the dropped candidates are never surfaced per query; consumer cannot tell a 0-caller result is incomplete.
- Remediation: emit "N ambiguous candidates dropped" in `up`/`blast` output when N>0.

## Not verified (no fresh evidence)
PageRank NaN/dangling handling, cycle termination, HashMap ordering determinism — prior-report claims (scope_2_graph_centrality.md, 2026-09-21) are stale vs v2.4.8.
