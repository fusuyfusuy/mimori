---
scope: "Domain Models, Workspace & Storage"
score: 9.1
status: "EXEMPLARY"
critical_findings: 0
invariant_breaches: []
---

# Scope 2: Domain Models, Workspace & Storage Audit

## Overview
Evaluated domain entities, workspace walker, path aliases, and SQLite storage: [`src/model/`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/model/) (`coordinate.rs`, `slice.rs`, `symbol.rs`), [`src/storage/`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/storage/) (`db.rs`, `sync.rs`), and [`src/workspace/`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/) (`aliases.rs`, `find.rs`, `walker.rs`).

## Findings

### 1. Robustness: Filter Flag Bypass in `execute_find` Literal Fallback (Minor)
- **Location**: [`src/workspace/find.rs#L152-L174`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/find.rs#L152-L174)
- **Detail**: In `execute_find`, when `matches.is_empty()`, the function executes a fallback workspace scan emitting literal line matches. However, this fallback runs even if the caller explicitly requested `--files` (`files_only = true`). A user filtering strictly for file paths unexpectedly receives literal line matches if no filename matches the query.

### 2. Efficiency: Redundant Database Handle in `execute_find` (Minor)
- **Location**: [`src/workspace/find.rs#L90-L107`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/find.rs#L90-L107)
- **Detail**: `execute_find` calls `get_or_sync_graph(root)?` (which opens the DB, syncs files, and loads all symbols), and immediately afterwards reopens `Database::open_or_create(&db_path)` to fetch file records. The file list is already indexed or derivable from the graph, making the second connection initialization redundant.

### 3. Invariant Compliance: FNV-1a Content Hashing & Transactional Invalidation (Exemplary)
- **Location**: [`src/workspace/walker.rs#L70`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/walker.rs#L70), [`src/storage/db.rs#L15-L94`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/storage/db.rs#L15-L94), [`src/storage/sync.rs#L37-L44`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/storage/sync.rs#L37-L44)
- **Detail**: Strict adherence to content-hash invalidation over filesystem `mtime`. Cache schemas enforce `PARSER_VERSION` pragma versioning and `alias_fingerprint` tracking to invalidate stale TSConfig alias resolutions automatically.

### 4. Security: Workspace Confinement Bounds (Exemplary)
- **Location**: [`src/workspace/walker.rs#L244-L286`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/workspace/walker.rs#L244-L286)
- **Detail**: `confine_to_workspace` enforces canonicalized path resolution against `workspace_root`, throwing explicit errors on directory traversal or escape attempts.

## Recommendations
1. In `src/workspace/find.rs`, condition the literal fallback on `!files_only`.
2. Reuse the active database connection or extract file lists directly from `SymbolGraph` in `execute_find`.
