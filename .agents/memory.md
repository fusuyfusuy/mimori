# Project Memory

## Active Epics & Scale
- Scale: Baseline architecture initialized.

## KNOWN DEBT (open only — one line per item, delete when done)
# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>

## Domain Vocabulary & Gotchas
- Symbol: Named code construct (function, struct, method, class, enum, interface, trait, type alias, or top-level variable) declared in source file.
- Slice: Isolated, token-dense view containing a symbol's exact source body, coordinates, signature, and immediate 1-hop dependencies.
- Caller: Upstream symbol or function that invokes, references, or depends on a target symbol.
- Callee: Downstream symbol or function that is invoked or referenced by a target symbol.
- Blast Radius: Transitive closure of upstream callers and entry points impacted by modification to a target symbol.
- Map: Hierarchical, ranked structural overview of top-level symbols, modules, and entry points across codebase.
- Coordinate: Unambiguous path and line identifier targeting a symbol or line range (e.g., `src/auth.rs:login` or `src/auth.rs:#L40-85`).
- Centrality: Graph-theoretic score measuring symbol's structural importance based on inbound dependency topology (In-Degree PageRank).
- Gotcha: `.mimori/` is ephemeral cache (gitignored); `.agents/` is living git-tracked domain memory.
- Gotcha: Ponytail technical debt comments (`# ponytail: <what> <- <ceiling> -> <upgrade_trigger>`) sync deterministically to `.agents/memory.md` with hard 30-item ceiling.
- Gotcha: Skill spec lives in two tracked copies (`SKILL.md`, `skills/mimori/SKILL.md`) plus the installed copy under `~/configs/agents-config/skills/mimori/`; editing one silently drifts the others.
