# AGENTS.md

Instructions for autonomous AI agents working in the `mimori` repository.

## Repository Overview

`mimori` is a zero-config, high-performance AST code-intelligence, symbol-graph, and context-slicing engine in Rust.

- **`src/parser/`**: Polyglot Tree-sitter parsers (Rust, TypeScript/JavaScript, Python, Go) extracting declarations, calls, imports, and references.
- **`src/graph/`**: Dependency graph construction, In-Degree PageRank centrality calculations, blast radius tracing, and architectural mapping.
- **`src/model/`**: Core types: `Symbol`, `Coordinate`, `Slice`, and language descriptors.
- **`src/storage/`**: Incremental SQLite cache (`.mimori/index.db`) keyed on file content hashes.
- **`src/workspace/`**: File traversal, `.gitignore` / `.mimoriignore` resolution, and monorepo path alias tracking.
- **`src/mcp/`**: Native JSON-RPC stdio Model Context Protocol server exposing `mimori_slice`, `mimori_map`, `mimori_find`, `mimori_blast`, and `mimori_graph`.
- **`src/cli/`**: Clap command-line parser and command dispatch.

## Build and Test

- **Build binary**: `cargo build --release`
- **Run all tests**: `cargo test`
- **Lint / format check**: `cargo clippy --all-targets` and `cargo fmt --check`

## Architecture Invariants

1. **Content-Hash Driven**: Never rely purely on filesystem timestamps (`mtime`). Re-indexing must be strictly governed by FNV-1a content hashes.
2. **Deterministic & Non-Interactive**: All commands must be non-interactive and return structured output (`--json`) or exit with clear non-zero codes on ambiguity.
3. **Workspace Confinement**: MCP tools and CLI operations must never escape the workspace root.
4. **Zero Background Daemons**: The binary operates strictly on-demand or as an MCP stdio server within the lifespan of an agent session.
