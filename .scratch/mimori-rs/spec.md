Status: ready-for-agent

# Specification: mimori-rs Code Intelligence Engine & CLI

## Problem Statement

When AI coding agents and developers navigate medium-to-large codebases, existing tools present a stark tradeoff:
1. Grep/Ripgrep-based search returns hundreds of noisy, unstructured line hits without structural context or callers/callees.
2. Full-file reads waste enormous token budgets (often 3,000+ tokens per file) to extract a single 15-line function.
3. Language Server Protocols (LSP) and compiler toolchains require complex runtime installations, fail when files are in half-written/dirty states, and cannot provide fast, zero-config cross-language answers in under 50 milliseconds.
4. Raw symbol dumps display files alphabetically rather than ranking by architectural centrality, forcing agents to sift through peripheral helpers to find core domain abstractions.

## Solution

`mimori-rs` is a zero-dependency, high-performance Rust CLI and agent tool that maps repository structure using Tree-sitter AST queries, calculates architectural centrality using in-degree PageRank, and provides one-shot retrieval commands (`find`, `slice`, `up`, `down`, `blast`, `map`). It produces token-dense Markdown tailored for LLM prompt context by default and structured JSON for programmatic pipelines, cached via binary serialization in `.mimori/` for sub-50ms query latency.

## User Stories

1. As an AI agent, I want to run `mimori-rs find <pattern>` to locate all symbols matching a query ranked by architectural centrality, so that I can immediately identify the primary definition without guessing.
2. As an AI agent, I want to run `mimori-rs slice <coordinate>` to receive the exact source code of a symbol (under 250 lines) along with its immediate 1-hop callers and callees, so that I consume under 150 context tokens instead of reading entire files.
3. As an AI agent, I want to run `mimori-rs slice <file>:#L<start>-<end>` to extract a specific line range with attached symbol context, so that I can inspect arbitrary blocks of code with full coordinate awareness.
4. As an AI agent, I want to run `mimori-rs up <target>` to discover all upstream callers that invoke or reference a symbol, so that I understand where a function is consumed before modifying its signature.
5. As an AI agent, I want to run `mimori-rs down <target>` to list all downstream callees invoked by a symbol, so that I understand its dependencies and internal call tree.
6. As an AI agent, I want to run `mimori-rs blast <target>` to evaluate the transitive ripple impact across all callers and entry points up to a configurable depth, so that I can anticipate breaking changes before editing code.
7. As an AI agent, I want to run `mimori-rs map` to view a ranked structural skeleton of the repository's modules and top-level symbols, so that I understand the system architecture within a tight token budget.
8. As an AI agent, I want to filter `mimori-rs map --scope <dir>` or `mimori-rs map --focus <target>` to restrict the architectural map to a specific subsystem, so that I can focus purely on relevant boundaries.
9. As an AI agent, I want to pass `--json` to any command to receive machine-readable JSON output, so that I can pipe results directly into automated scripts and subagent workflows.
10. As a developer, I want `mimori-rs` to automatically discover files while respecting `.gitignore`, `.ignore`, and standard ignore lists (`node_modules`, `target`, `vendor`, `.git`), so that I don't index generated artifacts or build output.
11. As a developer, I want `mimori-rs` to cache its parsed symbol graph in `.mimori/` using fast binary serialization with file mtime/hash checks, so that subsequent invocations complete in under 10 milliseconds.
12. As a developer, I want `mimori-rs clean` to purge cached graph data, so that I can force a full fresh re-index whenever necessary.
13. As an AI agent, I want `mimori-rs` to parse TypeScript, JavaScript, Python, Go, and Rust without requiring any external compilers or runtimes installed, so that it works out-of-the-box in any container or environment.
14. As an AI agent, I want `mimori-rs` to calculate in-degree PageRank centrality across all symbols, so that core domain traits and high-traffic utilities are prioritized over leaf helpers in map and search outputs.
15. As an AI agent, I want `mimori-rs slice --follow-local` to inline private local callee symbol bodies within the slice, so that I understand a function's helper logic without making multiple tool calls.

## Implementation Decisions

1. **Standalone Rust Binary with Statically Embedded Parsers**:
   - The tool is implemented in Rust as a standalone executable.
   - Tree-sitter parsers for TypeScript/JavaScript, Python, Go, and Rust are compiled directly into the binary via build scripts, ensuring zero external dynamic dependencies.
   *(See ADR-0001)*

2. **Syntactic AST Extraction & Heuristic Graph Construction**:
   - Tree-sitter query patterns extract declarations (functions, structs, methods, classes, enums, interfaces, traits, type aliases, top-level variables), imports/exports, and call sites.
   - Cross-file symbol resolution links call sites to declarations using import paths and lexical scoping heuristics.
   *(See ADR-0002)*

3. **In-Degree PageRank Centrality**:
   - The symbol graph is represented as a directed graph where edges flow from callers to callees (or reference dependencies).
   - In-degree PageRank is calculated via power iteration ($d = 0.85$, 25 iterations) during indexing to compute a centrality score for every symbol.
   - Maps and search listings rank symbols by centrality score descending.
   *(See ADR-0003)*

4. **Persistence & Incremental Cache**:
   - The indexed graph, symbol definitions, and centrality scores are persisted into `.mimori/index.db` via embedded SQLite (`rusqlite` bundled) with WAL mode.
   - File metadata (modification times and content hashes) enables atomic incremental updates per file, eliminating full-graph rewrites.

5. **Token-Dense Output Formatting**:
   - Default stdout output is formatted as concise Markdown (line coordinates, signatures, caller/callee bullet lists, symbol source blocks).
   - `--json` outputs structured schemas conforming to agent tool definitions.

6. **Coordinate Targeting & Disambiguation**:
   - Commands accept coordinates in the format `path/to/file:symbol` or `path/to/file:#Lstart-end`.
   - If a bare symbol name is provided (e.g. `mimori-rs slice authenticate`) and multiple matches exist, `mimori-rs` displays the ranked list of matching coordinates ordered by centrality.

## Testing Decisions

1. **Testing Philosophy**:
   - Tests must exercise external behavior through public seams rather than internal parser state or private AST nodes.
   - All tests run against realistic, multi-language fixture directory trees created in temporary folders.

2. **Testing Seams**:
   - **Primary Seam (CLI End-to-End)**: CLI integration tests executing the compiled `mimori-rs` binary (`assert_cmd`) against fixture repositories (TypeScript, Python, Rust, Go), asserting exit code 0, correct Markdown slice formatting, and valid JSON output.
   - **Engine Seam (Core Functional API)**: Direct unit/integration tests against the `SymbolGraph` engine (`parse_workspace`, `find_symbol`, `extract_slice`, `compute_pagerank`, `calculate_blast_radius`).

3. **Prior Art & Fixtures**:
   - Multi-language sample repositories with circular dependencies, multi-file imports, method shadowing, and large files (> 250 lines) to verify truncation and coordinate precision.

## Out of Scope

1. Dynamic code execution, type evaluation, or runtime debugging.
2. Full compiler-grade type checker resolution (e.g. resolving complex TypeScript conditional generics or Rust macro expansions).
3. Writing or modifying source code (this tool is strictly read-only and analytical).
4. Interactive GUI/TUI interface (pure CLI with Markdown and JSON stdout).

## Further Notes

- The binary will be packaged with an agent skill definition (`SKILL.md`) describing usage and prompt examples for AI agents.
- The CLI command structure matches the `mimori` convention: `init`, `map`, `slice`, `find`, `up`, `down`, `blast`, `clean`, `dump`.
