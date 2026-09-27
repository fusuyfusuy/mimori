---
scope: "External Interfaces: CLI & MCP Server"
score: 9.2
status: "EXEMPLARY"
critical_findings: 0
invariant_breaches: []
---

# Scope 5: External Interfaces (CLI & MCP Server) Audit

## Overview
Evaluated command-line argument dispatch, JSON formatting, JSON-RPC 2.0 stdio server, and MCP tool schemas: [`src/cli/args.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/cli/args.rs), [`src/cli/mod.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/cli/mod.rs), [`src/main.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/main.rs), [`src/mcp/protocol.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/protocol.rs), [`src/mcp/tools.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/tools.rs), and [`src/mcp/mod.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/mod.rs).

## Findings

### 1. Invariant Compliance: JSON-RPC 2.0 & Stdio Transport Isolation (Exemplary)
- **Location**: [`src/mcp/mod.rs#L50-L117`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/mod.rs#L50-L117), [`src/mcp/protocol.rs#L31-L53`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/protocol.rs#L31-L53)
- **Detail**: Background thread isolates stdin stream reading, properly handles notifications (`notifications/cancelled`), returns standard JSON-RPC `-32700` parse error codes, and formats all MCP tool responses with `content: [{type: "text", text: ...}], isError: false`. Diagnostics are logged exclusively to stderr, preserving stdio protocol framing integrity.

### 2. Workspace Confinement Across MCP & CLI (Exemplary)
- **Location**: [`src/mcp/tools.rs#L31-L71`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/tools.rs#L31-L71), [`src/main.rs#L808-L829`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/main.rs#L808-L829)
- **Detail**: Both CLI entry points and MCP tool invocations enforce strict workspace confinement via canonicalization and system directory protection (`is_system_directory`), preventing path traversal escapes or accidental system indexing.

### 3. Performance: In-Memory `McpCache` Fingerprinting Overhead (Minor)
- **Location**: [`src/mcp/tools.rs#L90-L120`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/mcp/tools.rs#L90-L120)
- **Detail**: `McpCache::get_graph` computes `compute_fingerprint` by scanning and hashing all workspace files on every tool call. While fast (<5ms) on small codebases, on very large repositories (>5,000 files) this can add noticeable per-tool latency. Adding a debounced or cached timestamp check before full hash iteration would optimize repeated rapid tool calls.

## Recommendations
1. Consider caching file scan metadata with a brief mtime pre-check in `compute_fingerprint` to avoid redundant full-repo content hashing across back-to-back MCP requests within the same second.
