# 5. Model Context Protocol (MCP) Server Interface

We decided to add a native `mimori mcp` subcommand implementing the Model Context Protocol (MCP) over `stdio` using standard JSON-RPC 2.0 (protocol version `2024-11-05`).

Autonomous coding agents evaluate declared tool schemas in their environment before considering shell commands. When `mimori` only exists as CLI instructions in markdown, agents frequently bypass AST slicing and fall back to broad file reads or text searches, pulling hundreds of unnecessary lines into context and causing severe token waste.

By exposing `mimori` as an MCP server with 5 core tools (`mimori_slice`, `mimori_map`, `mimori_find`, `mimori_blast`, `mimori_graph`), agents naturally choose structured AST slices and PageRank orientation over brute-force file reads. Furthermore, the persistent MCP daemon maintains a warm in-memory `SymbolGraph` cache across invocations, validating content hashes in &lt;1ms while retaining zero background daemon overhead outside agent sessions.
