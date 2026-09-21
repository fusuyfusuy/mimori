---
scope: "cross-boundary-seams"
score: 6.8
status: "CRITICAL"
contract_divergences: 8
---

# Cross-Boundary Seam & Contract Audit: mimori

## 1. Parser ↔ Storage & Graph Seams
- **Visibility Dropped at AST Boundary**:
  - *Parser side* (`src/parser/typescript.rs:418-420`, `src/parser/rust.rs:32`): Recognizes export and visibility AST nodes.
  - *Model side* (`src/model/symbol.rs:38-68`): `Symbol` struct lacks a `visibility` field. Public API exports and private local helpers are indistinguishable in-memory and in SQLite.
- **SymbolKind Serialization & Silent Parse Fallback**:
  - *Storage writer* (`src/storage/db.rs:195`): Writes `s.kind.as_str()` (`"type"`, `"module"`).
  - *Storage reader* (`src/storage/db.rs:286-301`): `parse_symbol_kind()` omits `"module"` from the match pattern, silently falling back to `_ => SymbolKind::Module`. Any corrupted kind string silently turns into `Module`.
  - *Serde contract* (`src/model/symbol.rs:3`): `SymbolKind` derives default `Serialize`/`Deserialize` (PascalCase: `"TypeAlias"`), diverging from SQLite's lowercase `"type"`.

## 2. MCP / CLI ↔ Domain Engine Seams
- **Dead `workspace_dir` Parameter in MCP Engine**:
  - *Tool schema* (`src/mcp/tools.rs:141-144, 230-233, 253-256`): Declares `workspace_dir` on `mimori_slice`, `mimori_blast`, and `mimori_graph`.
  - *Tool handler* (`src/mcp/tools.rs:432, 511, 558`): Calls `let _scope_dir = resolve_workspace_scope(...)`, but prefixes with `_` and discards it. Execution defaults to `session.root` (`src/mcp/tools.rs:456, 514, 560`).
- **Feature Gap Across Interfaces (`--numbered`)**:
  - *CLI interface* (`src/cli/args.rs:85-89`): Supports `-n, --numbered` in `SliceArgs`.
  - *MCP interface* (`src/mcp/tools.rs:136-146, 319-329`): `numbered` is omitted from `mimori_slice` schema and `SliceToolArgs`.
- **Output Data Shape Asymmetry (Markdown vs JSON)**:
  - *CLI engine* (`src/main.rs:113-116, 169-171, 208-210`): Emits structured JSON objects on `--json` for `up`, `down`, `uses`.
  - *MCP engine* (`src/mcp/tools.rs:569, 573, 577`): Emits human-oriented Markdown strings inside `content[0].text`, denying callers structured programmatic graphs.

## 3. Storage Schema ↔ In-Memory Models
- **Dead SQLite Column & Dead Index (`centrality`)**:
  - *SQLite schema* (`src/storage/db.rs:49, 60`): `centrality REAL DEFAULT 0.0` and `CREATE INDEX idx_symbols_centrality ON symbols(centrality DESC)`.
  - *Runtime graph* (`src/storage/db.rs:200`, `src/graph/pagerank.rs:121-123`): SQLite inserts `s.centrality` (which is always `0.0` at parse time). Centrality is recomputed in-memory in `SymbolGraph::new` and never written back (`UPDATE symbols` is absent). The SQLite index is completely dead.
- **Schema Column Name Divergence (`references_json` vs `calls`)**:
  - *SQLite schema* (`src/storage/db.rs:50, 181`): Column named `references_json`.
  - *In-memory model* (`src/model/symbol.rs:51`): Field named `calls`.
- **Untyped File Record**:
  - *Database contract* (`src/storage/db.rs:133`): Returns `HashMap<String, (i64, i64, String)>` instead of a typed `FileRecord` struct.

## 4. Documented Invariants vs Runtime Enforcement
- **Workspace Confinement Breach (CRITICAL)**:
  - *Contract* (`AGENTS.md: Architecture Invariants #3`): "MCP tools and CLI operations must never escape the workspace root."
  - *MCP enforcement* (`src/mcp/tools.rs:446`): Strictly calls `confine(&session.root, &full)`.
  - *CLI violation* (`src/main.rs:31-32`): `slice_line_coordinate(file, *start, *end, args.with_imports)` directly accesses any host path without confinement checks. `mimori slice /etc/passwd:#L1-3` reads and prints `/etc/passwd`.
  - *CLI root discovery* (`src/main.rs:754`): `find_workspace_root(coord.absolute_parent().as_deref(), cwd)` traverses up from foreign paths outside cwd.
- **Content-Hash Driven Invariant**:
  - *Enforcement* (`src/storage/sync.rs:41`, `src/workspace/walker.rs:69`): Strictly enforced via FNV-1a hashes. `mtime` is never trusted for cache invalidation.
- **Deterministic & Non-Interactive Invariant**:
  - *Enforcement* (`src/main.rs:20-717`): Guaranteed non-interactive exit codes (0 on success, 1 on error).
- **Zero Background Daemons Invariant**:
  - *Enforcement* (`src/main.rs:710`): Process lifetime strictly bound to CLI execution or stdio lifecycle.
- **MCP STDOUT Purity Invariant**:
  - *Enforcement* (`src/mcp/mod.rs:36`, `src/storage/sync.rs:87, 108`): MCP server never writes unformatted text to stdout; diagnostics and profiles use `eprintln!`.

## 5. Invariant Drift Between Tests & Implementation
- **MCP Request Cancellation Race Condition**:
  - *Implementation* (`src/mcp/mod.rs:50-113`): Stdio lines are read on a background thread while requests are processed synchronously on the main thread.
  - *Test drift* (`tests/cli_mcp.rs:769-856`): `test_mcp_whole_session_stdout_purity` step 5 sends a tool request followed immediately by `notifications/cancelled` and `ping`. On multi-core systems, if the main thread processes the tool request before the background thread parses the cancellation notification, the tool response is written to stdout. The test expecting `ping` receives the tool response, intermittently failing `assert_eq!(r5["id"], "ping_6")`.
- **Asymmetric Security Testing**:
  - *Test suite* (`tests/cli_mcp.rs:458-524`): Tests path confinement exclusively on the MCP interface.
  - *Gap*: CLI tests (`tests/cli_slice.rs`) lack path confinement assertions, allowing the CLI arbitrary filesystem read breach to remain undetected in CI.
