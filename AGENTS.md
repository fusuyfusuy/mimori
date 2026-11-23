# AGENTS.md

Instructions for autonomous AI agents working in the `mimori` repository.

## Repository Overview

`mimori` is a zero-config, high-performance AST code-intelligence, symbol-graph, context-slicing, and JIT in-repo agent memory engine in Rust.

- **`src/parser/`**: Polyglot Tree-sitter parsers (`rust`, `typescript`, `python`, `go` + shared `common`) extracting declarations, `SymbolFlags` (`IS_TEST`, `IS_PUBLIC`, `IS_TRAIT_IMPL`, `IS_ATTR_ENTRY`), local receiver-type hints, calls, imports, and references.
- **`src/graph/`**: Dependency graph construction, In-Degree PageRank centrality, module-stratified mapping (`map`), blast radius with unresolved-member honesty warnings (`blast`), git diff blast (`diff`), health audit (`doctor`), and absence sweeps (`missing`).
- **`src/model/`**: Core types: `Symbol` (`BTreeMap` `call_counts`, `flags`), `Coordinate`, `SliceResult` (typed `InlinedCallee`, `memory_notes`), and language descriptors.
- **`src/storage/`**: Atomic incremental SQLite cache (`.mimori/index.db`) keyed on FNV-1a content hashes, with corrupt auto-rebuild, read-only in-memory fallback, and POSIX stat-gated warm MCP fingerprinting.
- **`src/workspace/`**: Confined file traversal (`confine_path`, `WorkspaceScope`), `.gitignore` / `.mimoriignore` resolution, monorepo alias tracking (`AliasSet`), and hybrid search (`find`).
- **`src/memory/`**: Git-tracked memory ledger (`.agents/memory.md`) with JIT `@scope:`/`@sym:` scoped notes and `memory add`, Tree-sitter comment AST `mimodept:` debt scanner/sync (soft 30-item warning ceiling), `docs/adr/` title indexing, and budget-capped Turn-0 `dump`.
- **`src/mcp/`**: Native JSON-RPC 2.0 stdio Model Context Protocol server exposing 11 tools: `mimori_slice`, `mimori_dump`, `mimori_map`, `mimori_find`, `mimori_blast`, `mimori_graph`, `mimori_diff`, `mimori_missing`, `mimori_doctor`, `mimori_memory`, and `mimori_debt`.
- **`src/cli/`**: Clap command-line parser (with global `--workspace` and `--json`) and modular command dispatch.

## Build and Test

- **Build binary**: `cargo build --release`
- **Run all tests**: `cargo test`
- **Lint / format check**: `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`
- **Memory & debt self-check**: `mimori memory lint && mimori debt check`

## Architecture Invariants

1. **Content-Hash Driven**: Never rely purely on filesystem timestamps (`mtime`). Re-indexing is strictly governed by FNV-1a content hashes (`quick_stat_fingerprint` uses POSIX `ino+size+mtime+ctime` as a warm-MCP fast path only).
2. **Deterministic & Non-Interactive**: All commands are non-interactive, emit deterministic sorted outputs (`--json` and Markdown), and exit non-zero on ambiguity or missing symbols.
3. **Workspace Confinement**: MCP tools, CLI coordinates, `--scope` filters, and manifest parsers must never escape the workspace root or leak path-existence oracles.
4. **Zero Background Daemons**: The binary operates strictly on-demand or as an MCP stdio server within the lifespan of an agent session.
