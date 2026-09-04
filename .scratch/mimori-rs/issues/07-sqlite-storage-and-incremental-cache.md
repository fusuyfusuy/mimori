# 07: SQLite Storage, Instant Queries & Incremental Caching (clean)

**What to build:** Embedded SQLite database layer (`rusqlite` bundled) to persist the symbol graph, dependency edges, and PageRank scores in `.mimori/index.db`. Queries run sub-millisecond against indexed SQLite B-Trees without full-graph deserialization into RAM. When files are modified, only the changed files are re-parsed and updated incrementally in a single transaction. Also implements `mimori-rs clean [--all]`.

**Blocked by:** 05: In-Degree PageRank Centrality & Structural Code Map (map), 06: Transitive Ripple Impact / Blast Radius (blast)

**Status:** resolved

- [x] Embedded SQLite schema (`files`, `symbols`, `edges`, `meta`) with WAL mode enabled in `.mimori/index.db`.
- [x] Direct B-Tree indexed SQL queries powering `find`, `slice`, `up`, `down`, and `map` in under 1 millisecond.
- [x] Incremental file modification detector comparing file mtimes and SHA-256 hashes against the database.
- [x] Atomic incremental re-indexing transaction for modified or deleted files.
- [x] `mimori-rs clean` purges the `.mimori/index.db` cache to force a complete re-index.
- [x] Performance benchmarks and CLI tests verifying sub-millisecond query execution and incremental update correctness.
