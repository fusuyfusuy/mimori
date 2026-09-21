---
scope: "storage-and-workspace"
score: 9.1
status: "MINOR"
critical_findings: 0
invariant_breaches: []
---

# Scope 3 Audit: Storage Engine & Workspace Resolution (Post-Remediation)

Post-remediation verification audit of SQLite caching, FNV-1a hashing, incremental sync, workspace walker, path alias resolution, and symbol/file discovery.

## 1. Executive Summary & Scoring
- **Health Score**: **9.1 / 10.0** (Status: **MINOR**)
- **Target Seams**: [`src/storage/db.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs), [`src/storage/sync.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/sync.rs), [`src/storage/mod.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/mod.rs), [`src/workspace/walker.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/walker.rs), [`src/workspace/aliases.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs), [`src/workspace/find.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/find.rs), [`src/workspace/mod.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/mod.rs).
- **Critical Findings**: 0 | **Moderate Findings**: 0 | **Minor / Hygiene Findings**: 4
- **Invariant Breaches**: 0 (All previous breaches remediated and verified).

---

## 2. Verification of Remediations

1. **AliasSet::fingerprint Determinism**: **VERIFIED**
   - [`src/workspace/aliases.rs#L160-L170`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L160-L170): `all_paths` gathers manifests and `visited_configs`, sorts lexicographically (`all_paths.sort()`), and deduplicates (`all_paths.dedup()`). FNV-1a fingerprinting is 100% deterministic across processes, preventing spurious cache purges.
2. **Strict Workspace Confinement on `baseUrl`**: **VERIFIED**
   - [`src/workspace/aliases.rs#L503-L515`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L503-L515): `collect_base_url_aliases` canonicalizes both `root` and `base_dir`. If `!canon_base_dir.starts_with(&canon_root)`, it aborts prior to `std::fs::read_dir`, eliminating path traversal leaks.
3. **SQLite Immediate Transaction Locking**: **VERIFIED**
   - [`src/storage/db.rs#L166-L168`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs#L166-L168): `save_batch` invokes `self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)`. This acquires a `RESERVED` lock immediately in WAL mode, preventing concurrent lock upgrade deadlocks.
4. **Find Coverage for Symbol-less Files**: **VERIFIED**
   - [`src/workspace/find.rs#L94-L133`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/find.rs#L94-L133): `execute_find` queries `db.get_file_records()` from SQLite `files` table when `!symbols_only`, enabling file search (`-f` and general find) to locate files indexed with zero symbols.

---

## 3. Deep Dimension Findings

### 3.1 Correctness & Determinism
- [VERIFIED] **Deterministic Caching**: [`src/workspace/aliases.rs#L160-L170`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L160-L170) eliminates `HashSet` iteration non-determinism.
- [VERIFIED] **Symbol-less File Indexing**: Symbol-less files are stored in `files` table and matched in [`src/workspace/find.rs#L100-L133`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/find.rs#L100-L133).
- [MINOR] **Dotted Config Base Path**: In [`src/workspace/aliases.rs#L435-L437`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L435-L437), `target.extension().is_none()` evaluates to false on paths like `"tsconfig.base"` (`Some("base")`), skipping `.set_extension("json")`.
- [MINOR] **Multi-level Glob Expansion**: In [`src/workspace/aliases.rs#L252-L264`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L252-L264), `clean.ends_with("/**")` reads only the immediate parent directory rather than recursively walking subtrees.

### 3.2 Security & Workspace Confinement
- [VERIFIED] **BaseUrl Boundary Confinement**: Enforced via `starts_with(&canon_root)` check before directory read in [`src/workspace/aliases.rs#L509-L515`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/aliases.rs#L509-L515).
- [VERIFIED] **SQL Injection Immunity**: [`src/storage/db.rs#L28-L215`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs#L28-L215) uses compile-time parameters and strict SQL binding exclusively.

### 3.3 Robustness & Concurrency
- [VERIFIED] **WAL Upgrade Deadlock Elimination**: Immediate transactions prevent `SQLITE_BUSY` contention during parallel CLI/MCP execution.
- [MINOR] **Corrupt Database Auto-Recovery**: [`src/storage/db.rs#L22-L78`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs#L22-L78) lacks automated quarantine/rebuild for truncated or corrupt `.mimori/index.db` files.

### 3.4 Performance & Resource Hygiene
- [MINOR] **Literal Fallback Walk Redundancy**: When AST search matches 0 symbols, [`src/workspace/find.rs#L155`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/find.rs#L155) calls `scan_workspace(root)` for literal matching, executing an additional filesystem walk rather than reusing warm file buffers.
- [MINOR] **Write-Lock JSON Serialization**: In [`src/storage/db.rs#L188-L214`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs#L188-L214), `serde_json::to_string` runs sequentially inside the immediate write transaction.

---

## 4. Invariant Compliance Audit

| AGENTS.md Invariant | Status | Evidence / Analysis |
|---|---|---|
| **Content-Hash Driven** | **PASS** | [`src/storage/sync.rs#L40-L44`](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/sync.rs#L40-L44): Content hash governs invalidation. Fingerprint hashing is deterministic. |
| **Workspace Confinement** | **PASS** | Strict canonical root confinement verified across `confine_to_workspace`, walker, and `baseUrl` alias collector. |
| **Zero Background Daemons** | **PASS** | On-demand execution with synchronous SQLite connection management; no persistent background processes. |

---

## 5. Prioritized Actionable Recommendations
1. **Auto-Recovery for Corrupt DB** (P2): Wrap `Database::open_or_create` with a rescue block that backs up and recreates corrupt or zero-byte `index.db` files.
2. **Recursive Workspace Glob Walk** (P3): In `expand_workspace_globs`, recurse directory trees when pattern ends with `/**`.
3. **Pre-Serialize JSON Outside Write Tx** (P3): Perform `serde_json::to_string` in the Rayon parallel mapping stage in `sync.rs` prior to entering `save_batch`.
