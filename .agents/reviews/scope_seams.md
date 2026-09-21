---
title: "Scope 5 (Cross-Boundary Seams) Verification Audit"
date: "2026-09-21"
score: 10
status: "Exemplary"
---

# Scope 5: Cross-Boundary Seams Audit Report

## 1. MCP vs CLI Workspace Confinement
- **File**: `src/mcp/tools.rs#confine`
- **Verification**: Properly resolves both absolute and relative paths against `session_root`. Validates the canonicalized path by asserting `canon_candidate.starts_with(&canon_root)`, explicitly neutralizing path-traversal (`../`) attacks.
- **Status**: **Pass** (Strictly satisfies Architecture Invariant 3).

## 2. Parser -> Graph Seam
- **Files**: `src/graph/mod.rs`, `src/parser/python.rs`, `src/parser/go.rs`
- **Verification**:
  - `extract_file_imports` correctly scans up to 400 lines, ensuring large block comments or licenses don't truncate structural imports.
  - The Python AST parser selectively quarantines built-in dependencies (`os`, `sys`, etc.) into `external_imports` using the strict `PYTHON_STDLIB` list. This guarantees that non-standard library packages remain resolvable locally, preserving cross-file edges.
  - The Go AST parser uses a dotless-path heuristic (`!p.contains('.')`) to designate standard library imports as `external_imports`, which elegantly allows dot-qualified modules (e.g., `github.com/...`) to retain their cross-file mappings.
- **Status**: **Pass** (Cross-file edges cleanly maintained across language domains).

## 3. Storage -> Model Seam
- **Files**: `src/storage/db.rs`, `src/storage/sync.rs`
- **Verification**:
  - Database writes batch cleanly inside an `Immediate` SQLite transaction to avert race conditions or partial schema violations.
  - Storage invalidation expressly discards `mtime` as a sole indicator (`sync.rs:43`), enforcing deterministic structural validation against the content `hash` to prevent silent desyncs from `rsync` or `touch`.
- **Status**: **Pass** (Highly deterministic caching mechanics).

## 4. Memory & Debt Seams
- **Files**: `src/memory/debt.rs`, `src/memory/ledger.rs`
- **Verification**:
  - Scope confinement in `scan_debt_markers` confirms `canon_joined.starts_with(&canon_root)` before returning markers.
  - Pattern validation strictly adheres to `AGENTS.md` instructions. The `parse_debt_line` extracts the exact format (`- <what> <- <why> -> <trigger>`).
  - Strikethroughs (`~~`) and checked items (`[x]`) are hard-rejected in `ledger.lint()`, rigorously enforcing the "Open-only" requirement, and properly honoring the `MAX_DEBT_CEILING` boundary of 30.
- **Status**: **Pass** (Exemplary adherence to the behavioral ledger model).

## Final Score
**10 (Exemplary)**
No remediation required. Cross-boundary constraints operate reliably across graph, caching, language parsing, and conversational ledger contexts.
