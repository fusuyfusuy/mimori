---
scope: "Project Memory & Technical Debt Ledger"
score: 9.3
status: "EXEMPLARY"
critical_findings: 0
invariant_breaches: []
---

# Scope 4: Project Memory & Technical Debt Ledger Audit

## Overview
Evaluated project memory substrate, ponytail debt scanner, M2M linter, and Turn-0 context packer: [`src/memory/mod.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/mod.rs), [`src/memory/debt.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/debt.rs), [`src/memory/dump.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/dump.rs), and [`src/memory/ledger.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/ledger.rs).

## Findings

### 1. Robustness: Overly Permissive Debt Section Matcher (Minor)
- **Location**: [`src/memory/debt.rs#L396-L398`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/debt.rs#L396-L398)
- **Detail**: `replace_debt_section` checks if a section heading contains `"known debt"` or `"debt"`. If a repository defines another section earlier in `.agents/memory.md` (e.g., `## Technical Debt Discussions`), the sync engine would replace the contents of that section instead of the deliberate `## KNOWN DEBT` ledger. Matching the explicit `known debt` title or anchoring to the exact M2M heading is more deterministic.

### 2. Invariant Compliance: 30-Item Hard Ceiling & Format Verification (Exemplary)
- **Location**: [`src/memory/ledger.rs#L6-L62`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/ledger.rs#L6-L62), [`src/memory/debt.rs#L371-L381`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/debt.rs#L371-L381)
- **Detail**: Strict enforcement of `MAX_DEBT_CEILING = 30`. Format verification validates the `- <what> <- <why> -> <trigger>` Hoare triple contract and rejects markdown strikethroughs (`~~item~~`) or completed checkboxes (`- [x]`) to keep debt open-only.

### 3. Context Packing & Truncation Bounds in `dump` (Exemplary)
- **Location**: [`src/memory/dump.rs#L74-L115`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/memory/dump.rs#L74-L115)
- **Detail**: `generate_dump` calculates character budgets based on tokens (`div_ceil(4)`), dynamically calculates PageRank symbol limits, and truncates cleanly with `… [truncated to fit token budget]` markers, preventing context overflow during Turn-0 agent warmup.

## Recommendations
1. Tighten the debt section heading detection in `src/memory/debt.rs` to require `heading.contains("known debt")` rather than naked `debt`.
