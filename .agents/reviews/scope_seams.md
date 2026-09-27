---
scope: "cross-boundary-seams"
score: 8.4
status: "MODERATE"
contract_divergences: 2
---

# Scope 6: Cross-Boundary Seams & Contract Interfaces Audit

## Overview
Evaluated cross-boundary contracts and interface invariants across Parsers, Models, Storage, Graph Analytics, Memory, CLI, and MCP stdio interfaces.

## Findings

### 1. Seam Divergence: Parser Mention Collection vs Graph Consumer Contract (Moderate)
- **Location**: [`src/parser/python.rs#L139`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/python.rs#L139), [`src/parser/go.rs#L209`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/go.rs#L209) vs [`src/graph/blast.rs#L244`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/blast.rs#L244), [`src/graph/mod.rs#L555`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/graph/mod.rs#L555)
- **Detail**: The `Symbol` schema contract defines `pub mentions: Vec<String>` to power non-call mention tracking (`mimori uses` in CLI and `mimori_graph(direction="uses")` in MCP, plus `value_uses` in `mimori blast`). While `src/parser/typescript.rs` and `src/parser/rust.rs` extract mentions (arguments, property reads, template strings, type references), `python.rs` and `go.rs` instantiate an empty `mentions = Vec::new()` and never populate it. Downstream consumers assume uniform language capability, but value-mention tracking silently yields 0 hits for Python and Go codebases.

### 2. Contract Drift: CLI Filter Flag Intent vs Fallback Execution Seam (Minor)
- **Location**: [`src/cli/args.rs#L97-L104`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/cli/args.rs#L97-L104) vs [`src/workspace/find.rs#L152-L174`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/find.rs#L152-L174)
- **Detail**: The CLI contract specifies `--files-only` (`-f`) to restrict queries to file paths. When zero files match the query, `execute_find` invokes an unconditioned literal code content fallback. This violates the caller's explicit filter contract by returning code line snippets when only file paths were requested.

### 3. Persistence Schema Contract: SQLite Table vs In-Memory `Symbol` (Exemplary)
- **Location**: [`src/storage/db.rs#L41-L57`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/storage/db.rs#L41-L57), [`src/model/symbol.rs#L10-L24`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/model/symbol.rs#L10-L24)
- **Detail**: Zero contract drift between SQLite columns and Rust struct members across all 13 fields. All JSON serializations handle decoding errors explicitly via `rusqlite::Error::FromSqlConversionFailure` without silent `unwrap_or_default` fallback masking. `PARSER_VERSION = 7` pragma guarantees schema migration safety.

### 4. Architectural Invariant Enforcement: `AGENTS.md` vs Runtime Paths (Exemplary)
- **Location**: `AGENTS.md` rules vs [`src/workspace/walker.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/walker.rs), [`src/storage/sync.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/storage/sync.rs), [`src/main.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/main.rs)
- **Detail**:
  - **Content-Hash Invariant**: Verified. Invalidation relies purely on FNV-1a content hashes (`mtime` is never trusted).
  - **Deterministic Non-Interactive Invariant**: Verified. Unambiguous `--json` and deterministic non-zero exit codes.
  - **Workspace Confinement Invariant**: Verified. Both CLI and MCP paths enforce `confine_to_workspace` and system directory guards.
  - **Zero Background Daemons**: Verified. Stdio and CLI operate on-demand.
  - **Skill Spec Synchronization**: Verified. Tracked copies (`SKILL.md`, `skills/mimori/SKILL.md`) and the active installation copy are bit-for-bit identical.

## Recommendations
1. Populate `mentions` in `python.rs` and `go.rs` parsers to restore cross-language contract parity.
2. In `src/workspace/find.rs`, suppress the literal line search fallback when `files_only` is true.
