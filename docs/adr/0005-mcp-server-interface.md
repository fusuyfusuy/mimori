# 5. Model Context Protocol (MCP) Server Interface

We decided to add a native `mimori mcp` subcommand implementing the Model Context Protocol (MCP) over `stdio` using standard JSON-RPC 2.0 (protocol version `2024-11-05`).

Autonomous coding agents evaluate declared tool schemas in their environment before considering shell commands. When `mimori` only exists as CLI instructions in markdown, agents frequently bypass AST slicing and fall back to broad file reads or text searches, pulling hundreds of unnecessary lines into context and causing severe token waste.

By exposing `mimori` as an MCP server with core tools (`mimori_slice`, `mimori_map`, `mimori_find`, `mimori_blast`, `mimori_graph`, and subsequently `mimori_memory` and `mimori_debt`), agents naturally choose structured AST slices and PageRank orientation over brute-force file reads. Furthermore, the persistent MCP daemon maintains a warm in-memory `SymbolGraph` cache across invocations, validating content hashes in <1ms while retaining zero background daemon overhead outside agent sessions.

## Addendum (2026-09-14, v2.4.0)
Amended by ADR-0006 to add `mimori_memory` and `mimori_debt` to the MCP tool registry, giving agents native, non-destructive access to the project memory substrate and debt gates.

## Addendum (2026-09-21, v2.4.3)
Added `mimori_dump` to the registry and made `numbered` an explicit `mimori_slice` parameter.

## Addendum (2026-11-23, ADR-0007)
Expanded MCP registry to 11 tools (`mimori_slice`, `mimori_dump`, `mimori_map`, `mimori_find`, `mimori_blast`, `mimori_graph`, `mimori_diff`, `mimori_missing`, `mimori_doctor`, `mimori_memory`, `mimori_debt`); added stat-gated LRU graph caching (`ino+size+mtime+ctime` fast check with FNV-1a verification), arbitrary header ordering & bounded Content-Length framing, `mimori_memory(action="add")`, and session-root cache reuse for subdirectory `workspace_dir` scopes.
