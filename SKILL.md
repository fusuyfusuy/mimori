---
name: mimori
description: "High-performance code intelligence CLI: AST slicing, symbol search, dependency traversal, PageRank architectural mapping, and action journaling."
---

# MIMORI(1) — General Commands Manual

## NAME
**`mimori`** — zero-config AST code-intelligence, symbol-graph, and context-slicing engine

## SYNOPSIS
```shell
mimori init
mimori map    [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice  <coordinate> [-f|--follow-local] [-i|--with-imports] [--json]
mimori find   <pattern> [-s|--symbols-only] [-f|--files-only] [--json]
mimori up     <target> [--json]
mimori down   <target> [--json]
mimori blast  <target> [-d|--depth <N>] [--json]
mimori dump   [--file] [--scope <dir>] [--seed <term>] [--limit <N>] [--json]
mimori log    -a|--action <slug> -s|--summary <text> [-f|--files <f1,f2>] [--json]
mimori clean  [--all]
```

## DESCRIPTION
**`mimori`** provides token-dense structural code intelligence for AI agents and developers. It statically embeds Tree-sitter parsers for Rust, TypeScript/JavaScript, Python, and Go, indexing functions, methods, classes, traits, interfaces, exported constants, builder patterns, and object literal members (e.g. tRPC routers, Drizzle tables, Hono routes). It builds an in-memory cross-file dependency graph, computes architectural centrality via in-degree PageRank, and persists parsed symbols into an embedded SQLite database (`.mimori/index.db`). Indexing is near-linear in workspace size; a warm run on a 481,200-symbol workspace takes ~1.8s, and small repositories index in milliseconds.

All commands output compact Markdown optimized for LLM prompt context windows by default, or machine-readable JSON when `--json` is specified.

---

## GLOBAL OPTIONS
* `--json`  
  Emit all query results as structured JSON instead of human/LLM-readable Markdown.
* `-h`, `--help`  
  Print help information.
* `-V`, `--version`  
  Print version information.

---

## SUBCOMMANDS

### `map`
```shell
mimori map [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
```
Generate a hierarchical, centrality-ranked structural outline of codebase symbols, modules, and entry points.
* `--scope <dir>`: Restrict the map to files within the specified directory.
* `--focus <target>`: Run Personalized PageRank (PPR) biased toward `<target>` to surface its relevant architectural neighborhood.
* `--seed <term>`: Bias the ranking toward symbols whose name or file matches the term.
* `--limit <N>`: Keep only the top `N` symbols by centrality.

### `slice`
```shell
mimori slice <coordinate> [-f|--follow-local] [-i|--with-imports] [--json]
```
Extract an isolated, token-dense view containing a symbol's exact source body, coordinates, signature, and immediate 1-hop dependencies.
* `<coordinate>`: Target identifier (see **COORDINATE SYNTAX** below).
* `-f`, `--follow-local`: Inline private local callee symbol bodies declared within the same file.
* `-i`, `--with-imports`: Include top-of-file import statements in the slice header for direct dependency context without needing extra file reads.
* *Note*: Bodies exceeding 250 lines are cleanly truncated with head/tail excerpts to preserve token budgets.

### `find`
```shell
mimori find <pattern> [-s|--symbols-only] [-f|--files-only] [--json]
```
Search for symbols and files across the repository, ordered by exact match and in-degree PageRank centrality.
* `-s`, `--symbols-only`: Restrict search hits strictly to symbol declarations.
* `-f`, `--files-only`: Restrict search hits strictly to file paths.
* *Hybrid Fallback*: When zero symbols **and** files match, falls back to a case-insensitive literal scan of indexed symbol **bodies**, locating configuration tokens, string literals, and builder patterns. Text outside any symbol body -- imports, module-level statements, comments between declarations -- is not covered; use `rg` for those.

### `up`
```shell
mimori up <target> [--json]
```
Display all upstream **callers** (functions, methods, types) that invoke, reference, or depend upon `<target>`.

### `down`
```shell
mimori down <target> [--json]
```
Display all downstream **callees** (functions, methods, types) invoked or referenced by `<target>`.

### `blast`
```shell
mimori blast <target> [-d|--depth <N>] [--json]
```
Evaluate the transitive **blast radius** (ripple impact) when `<target>` changes. Traverses the upstream reachability closure up to depth `N` (default: 3), reporting affected callers, entry points, and test suites. An entry point is a symbol with no callers, or one named `main`; test detection matches real conventions (`tests/`, `__tests__/`, `_test.go`, `.spec.ts`, `test_*`) rather than any path containing the substring "test".

### `dump`
```shell
mimori dump [--file] [--scope <dir>] [--seed <term>] [--limit <N>] [--json]
```
Emit a full repository context snapshot. Combines the centrality-ranked repository map with the recent action history from `.mimori/activity.jsonl`.
* `--file`: Write output to `.mimori/.cache/context.md` instead of stdout.
* `--scope <dir>`: Restrict the snapshot to files within the specified directory.
* `--seed <term>`: Bias the ranking toward symbols whose name or file matches the term.
* `--limit <N>`: Keep only the top `N` symbols by centrality.

### `log`
```shell
mimori log -a|--action <slug> -s|--summary <text> [-f|--files <f1,f2>] [--json]
```
Append a high-signal action record to `.mimori/activity.jsonl` (<160 chars). Records discrete progress steps across turns, compacted sessions, and subagents.
* `-a`, `--action <slug>`: Action identifier (e.g. `jwt-rotation`, `fix-query`).
* `-s`, `--summary <text>`: Concise summary explanation (<160 chars).
* `-f`, `--files <paths>`: Comma-separated list of affected files.

### `init`
```shell
mimori init
```
Initialize the `.mimori` workspace directory and cache storage.

### `clean`
```shell
mimori clean [--all]
```
Purge the embedded SQLite cache (`.mimori/index.db`, WAL, SHM) to force a fresh re-index on the next command. If `--all` is passed, also removes `.mimori/.cache/`.

---

## COORDINATE SYNTAX

Commands accept coordinates in three formats:
1. **Symbol Coordinate**: `path/to/file:<symbol>`  
   Targets a specific declaration within a file (e.g., `src/auth.rs:authenticate` or `src/service.ts:UserService::findUser`).
2. **Line Coordinate**: `path/to/file:#L<start>-<end>`  
   Targets a precise line slice (e.g., `src/main.rs:#L20-45`).
3. **Bare Symbol**: `<symbol>` or `<Type>::<method>`  
   Lookup by name across the workspace.

### Resolution order

A file coordinate is matched in tiers, and the first tier that produces candidates wins:

1. exact workspace-relative path (absolute paths normalize into this tier),
2. path suffix on a **component** boundary (`alpha/mod.rs` matches `src/alpha/mod.rs`, `ha/mod.rs` does not),
3. basename alone.

**If any tier matches more than one symbol, the command exits `1` and prints the candidates
ranked by centrality.** `mimori` never silently picks between two files sharing a basename
such as `mod.rs`, `index.ts`, or `__init__.py`.

---

## ARCHITECTURAL PRINCIPLES

### Centrality & In-Degree PageRank
Rather than dumping symbols alphabetically or in source order, `mimori` models caller $\to$ callee dependency topology and computes in-degree PageRank via power iteration ($d = 0.85$, 25 iterations). Foundational abstractions (traits, types, shared utilities) rank highest, ensuring token budgets are spent on architectural backbones rather than leaf helpers.

### Persistence & Incremental Synchronization
Parsed symbols, coordinates, and references are persisted into `.mimori/index.db` using embedded SQLite with WAL (Write-Ahead Logging). Centrality is recomputed on load rather than stored.

Re-parsing is decided by **FNV-1a content hash**, not by timestamp: every source file is read and hashed on every run, and only files whose hash changed are re-parsed. Trusting mtime would let `cp -p`, `touch -r`, `rsync -t` or a `tar` extraction leave the index permanently stale. Reading and hashing costs roughly 0.4% of a warm run.

The index is derived data, safe to delete at any time, and rebuilds automatically when the embedded parser version changes.

---

## AGENT NAVIGATION WORKFLOW

When exploring or modifying a codebase, AI agents should follow the **Canopy $\to$ Slice $\to$ Blast $\to$ Log** discipline:

1. **Canopy (Orientation)**:
   ```shell
   mimori map --scope <dir> --limit 100
   ```
   Inspect the high-centrality symbols and entry points of the target subsystem without reading raw files. `--limit` caps the output at the top `N` symbols by centrality, which is what keeps a large workspace inside a token budget.

2. **1-Hop Slice (Inspection)**:
   ```shell
   mimori slice <file:symbol> -f
   ```
   Retrieve the exact target symbol body along with its 1-hop callers, callees, and inlined private helpers. Consumes ~120 tokens vs 3,000+ for whole-file reads.

3. **Blast Radius (Pre-edit Check)**:
   ```shell
   mimori blast <file:symbol>
   ```
   Identify all upstream callers, public entry points, and test suites that could break before editing a signature or contract.

4. **Action Journaling (Execution Tracking)**:
   ```shell
   mimori log -a "oauth-refresh" -s "Migrated token refresh to Redis with 5m TTL" -f "src/auth.ts,src/redis.ts"
   ```
   Record discrete milestones so subsequent turns and compacted sessions retain a high-signal action trail.

---

## EXAMPLES

### 1. Warm up workspace and snapshot context
```shell
mimori init
mimori dump --file
```

### 2. Search for high-centrality symbols
```shell
mimori find "authenticate" -s
```

### 3. Extract 1-hop AST context slice with inlined private helpers and imports
```shell
mimori slice src/auth/service.rs:authenticate -f -i
```

### 4. Traverse dependency callers and callees
```shell
mimori up src/auth/service.rs:authenticate
mimori down src/auth/service.rs:authenticate
```

### 5. Check blast radius before refactoring a core function
```shell
mimori blast src/db/connection.rs:query -d 3
```

### 6. Focus architectural map on a specific subsystem
```shell
mimori map --scope src/payment --focus PaymentGateway
```

### 7. Record an execution milestone in the journal
```shell
mimori log -a "db-migration" -s "Added users table migration and index" -f "migrations/0001_users.sql"
```

### 8. Force full re-indexing of the repository
```shell
mimori clean --all
```

---

## FILES
* `.mimori/index.db`: Embedded SQLite database storing file records, parsed symbols, and PageRank centrality scores.
* `.mimori/index.db-wal`: SQLite write-ahead log.
* `.mimori/activity.jsonl`: Append-only JSONL log of discrete agent actions recorded via `mimori log`. Rotates to `activity.jsonl.1` past 1 MiB.
* `.mimori/.cache/context.md`: Persisted context dump snapshot generated by `mimori dump --file`.
* `.mimoriignore`: Optional ignore file supplementing `.gitignore` for custom exclusion patterns.

---

## ENVIRONMENT
* `MIMORI_PROFILE`: When set to any value, prints per-phase indexing timings to stderr. Costs nothing when unset.

---

## EXIT STATUS
* `0`: Success.
* `1`: Symbol not found, ambiguous coordinate, coordinate parse error, unreadable workspace, or database error.
* `2`: Invalid command-line arguments.

---

## SEE ALSO
`rg`(1), `tree-sitter`(1), `git`(1)
