---
scope: "Storage & Workspace"
score: 6.8
status: "CRITICAL"
critical_findings: 3
invariant_breaches:
  - "Workspace Confinement (AGENTS.md Invariant 3): aliases.rs follows extends, references, and workspace globs outside workspace root without boundary validation."
---

# Storage & Workspace Audit Report

## 1. Executive Summary
Audit of `mimori` storage and workspace subsystems (`src/storage/{mod,db,sync}.rs`, `src/workspace/{mod,walker,aliases,find}.rs`). While the FNV-1a content-hash invalidation model correctly rejects pure `mtime` assumptions, critical vulnerabilities exist in path cycle detection (causing stack overflow crashes), workspace boundary confinement (violating Architecture Invariant 3), and SQLite concurrency resilience (zero busy timeout). Significant performance bottlenecks also affect transaction batching and filesystem traversal.

## 2. Dimension Evaluation

### 2.1 Correctness
- **FNV-1a vs mtime Invariants**: Compliant with Invariant 1. Invalidation in `sync.rs:41` relies on `scan.hash == db_hash`. `mtime` is stored (`files.mtime`) but not trusted for freshness.
- **Migration & Schema**: Parser version invalidation in `db.rs:81-93` cleans `files` and bumps `PRAGMA user_version`. However, `db.rs:49,60,200` defines a `centrality` column and index `idx_symbols_centrality` that is permanently 0.0—`SymbolGraph::new` calculates PageRank in memory and never writes it back to SQLite.
- **Ignore Filtering**: `walker.rs:147-156` configures `WalkBuilder` with `.hidden(true)`, `.git_ignore(true)`, and `.mimoriignore`. However, `walker.rs:94-102` applies `is_ignored_rel` only on files, not directories, failing to prune directory descent.
- **Alias Resolution**: `aliases.rs:207-232` only parses YAML package lists using line-based heuristics; flow-style arrays (`packages: ["..."]`) or inline definitions fail. `expand_workspace_globs` (`aliases.rs:249-260`) treats `/**` as a non-recursive immediate directory read.

### 2.2 Robustness
- **SQLite Concurrency & Busy Timeout**: Critical defect. `db.rs:27-32` configures WAL mode, normal synchronous, and foreign keys, but NEVER configures `busy_timeout` (default 0ms). Any concurrent read/write or parallel CLI/MCP invocation immediately errors with `database is locked`.
- **Corruption Recovery**: `db.rs:22` and `sync.rs:19` lack auto-recovery. A malformed or truncated SQLite database immediately halts commands without attempting fallback rebuild or database re-initialization.
- **Cycle Detection & Symlinks**: Critical defect. `aliases.rs:340,408-430` uses `HashSet<PathBuf>` without path canonicalization. Cross-directory mutual `extends` (`a/tsconfig.json` <-> `../b/tsconfig.json`) creates infinitely expanding paths (`a/../b/../a/...`), exhausting stack space and crashing with SIGSEGV.

### 2.3 Performance
- **Single-Item SQLite Transactions**: `sync.rs:68-83` saves each parsed file in an isolated transaction (`db.rs:163`), committing hundreds of individual transactions sequentially instead of a single batch transaction. Deletions (`db.rs:214`) run individual auto-commit DELETE statements.
- **Triple Workspace Walk**: `sync.rs:24-26` triggers three sequential directory tree walks: `discover_package_manifests`, `discover_tsconfigs`, and `scan_workspace_with_stats`.
- **Eager Memory Ingestion**: `walker.rs:64-77` loads the full text content of every supported source file in the repository into memory simultaneously on every invocation.
- **Duplicate Walk on Find Fallback**: `find.rs:137-139` invokes `scan_workspace` a second time when symbol/file search yields zero hits.

### 2.4 Security
- **Path Traversal / Workspace Confinement**: Critical breach of Invariant 3. `aliases.rs:408-413` (`follow_extends`), `aliases.rs:441-454` (`follow_reference`), and `aliases.rs:262` (`expand_workspace_globs`) resolve absolute paths or `..` traversals without ensuring target paths remain within `root`.
- **Arbitrary File Hashing**: `aliases.rs:160-167` reads and hashes any external file referenced by `tsconfig.json` extends/references into the SQLite alias fingerprint.
- **SQL Injection**: No vulnerabilities found. All dynamic queries in `db.rs:123,166,169,176,216` use parameterized `params![]`.

## 3. Findings Matrix

| Ref | Severity | File & Lines | Description |
|---|---|---|---|
| SEC-01 | CRITICAL | `src/workspace/aliases.rs:408-430,441-454` | Traversal outside workspace root via `extends` / `references` (Breaches Invariant 3). |
| ROB-01 | CRITICAL | `src/workspace/aliases.rs:339-342,408-430` | Stack overflow / SIGSEGV on cross-directory mutual `extends` due to unnormalized path cycle tracking. |
| ROB-02 | CRITICAL | `src/storage/db.rs:27-32` | Missing `busy_timeout` leads to immediate `SQLITE_BUSY` errors during concurrent access. |
| PERF-01 | MODERATE | `src/storage/sync.rs:68-83`, `src/storage/db.rs:163` | File updates committed individually per-file; lacks single batch transaction wrapper. |
| PERF-02 | MODERATE | `src/storage/sync.rs:24-26`, `src/workspace/aliases.rs:101-104` | Three separate full filesystem tree walks executed on every `get_or_sync_graph`. |
| PERF-03 | MODERATE | `src/workspace/find.rs:137-139` | Full workspace re-walk and file re-read triggered on literal find fallback. |
| CORR-01 | MINOR | `src/storage/db.rs:49,60,200` | Dead schema: `symbols.centrality` is never populated or indexed with calculated PageRank. |
| CORR-02 | MINOR | `src/workspace/walker.rs:94-102` | Directory ignore check occurs after `path.is_file()`, failing to prune unignored directory trees early. |
| CORR-03 | MINOR | `src/workspace/aliases.rs:249-260` | Glob expansion for `/**` does not perform recursive descent. |

## 4. Remediation Checklist
1. [ ] **Normalize Paths for Cycle Detection**: In `aliases.rs`, canonicalize or normalize `PathBuf` (e.g., via `dunce::canonicalize` or resolving `..` components) before inserting into `visited`.
2. [ ] **Enforce Workspace Root Confinement**: In `aliases.rs`, verify `target.starts_with(root)` before reading `extends`, `references`, or workspace glob paths.
3. [ ] **Configure SQLite Busy Timeout**: In `db.rs:open_or_create`, execute `PRAGMA busy_timeout = 5000;` or set `conn.busy_timeout(Duration::from_millis(5000))`.
4. [ ] **Batch Database Writes**: Add `save_files_and_symbols_batch(&mut self, &[...])` in `db.rs` to wrap all parsed file inserts and file deletions in a single transaction.
5. [ ] **Consolidate Discovery Walks**: Merge `discover_package_manifests`, `discover_tsconfigs`, and `discover_workspace_files` into a single `WalkBuilder` traversal pass with `.filter_entry()` directory pruning.
6. [ ] **Reuse Scans in Find Fallback**: Pass existing `FileScan` results into `find.rs` instead of re-invoking `scan_workspace`.
