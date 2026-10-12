# Master Architectural Audit Report — mimori v2.4.8 (2026-10-12)

> [!WARNING]
> Swarm dispatch failed (6/6 subagents: RESOURCE_EXHAUSTED 429). Re-ran as a single-agent pass with targeted live probes. **Parsers and Storage/Workspace scopes are `[DEGRADED / NOT RE-AUDITED]`**; their reports dated 2026-09-21 / 09-27 are stale against v2.4.8.

Baseline: `cargo build --release` ok, `cargo clippy --all-targets` clean, `cargo test` all pass.

## Scorecard

| Scope | Score | Status | Findings | Report |
|---|---|---|---|---|
| Parsers & Model | — | DEGRADED | not re-audited | scope_1_parsers_models.md (stale) |
| Graph Engine | 7.8 | MODERATE | 1 mod, 1 minor | [scope_2_graph.md](file:///home/devhax/projects/fusuyfusuy/mimori/.agents/reviews/scope_2_graph.md) |
| Storage & Workspace | — | DEGRADED | not re-audited | scope_3_storage_workspace.md (stale) |
| Memory & Debt | 6.9 | CRITICAL | 1 crit, 1 mod, 1 minor | [scope_4_memory_debt.md](file:///home/devhax/projects/fusuyfusuy/mimori/.agents/reviews/scope_4_memory_debt.md) |
| CLI & MCP | 8.0 | MODERATE | 1 mod, 2 minor | [scope_5_interfaces.md](file:///home/devhax/projects/fusuyfusuy/mimori/.agents/reviews/scope_5_interfaces.md) |
| Seams | 7.6 | MODERATE | 3 | [scope_seams.md](file:///home/devhax/projects/fusuyfusuy/mimori/.agents/reviews/scope_seams.md) |

## Roadmap

**P1 — data loss / crash**
1. M1: [debt.rs#L364-L371](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/debt.rs#L364-L371) `debt sync` silently deletes hand-written ledger debt lines (reproduced).
2. I1: [mcp/mod.rs#L52-L59](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L52-L59) unbounded line buffer; 200 MB line -> 200 MB RSS.

**P2 — invariants / correctness**
3. G1: self/receiver method calls unresolved -> `doctor` false dead-code, `blast` false negatives (3651 unresolved vs 775 resolved).
4. M2: [debt.rs#L262-L310](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/debt.rs#L262-L310) `debt check` ignores unsynced markers.
5. G2/M3: surface ambiguous-dropped counts; non-silent ceiling truncation.

**P3 — polish**
6. I2: expose `uses`/`missing`/`doctor` via MCP. 7. S3: resync installed SKILL.md copy.

## Unverified
Storage hash/DDL/concurrency, parser panics/UTF-8, PageRank edge cases.
