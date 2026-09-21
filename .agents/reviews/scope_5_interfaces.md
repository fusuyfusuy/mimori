---
scope: "External Interfaces (CLI & MCP)"
score: 7.9
status: "MODERATE"
critical_findings: 1
invariant_breaches:
  - "Invariant 2: mimori --json memory --section <sec> prints plain text markdown instead of structured JSON"
---

# Scope 5 Audit: External Interfaces (CLI & MCP Server)

## Executive Summary
Audit of the CLI entrypoints, library API, and native MCP stdio server reveals a robust, clean stdout-isolated architecture with strict path confinement. However, critical flaws exist in MCP request cancellation synchronization—causing an intermittent test failure in [`test_mcp_whole_session_stdout_purity`](file:///home/devhax/projects/fusuyfusuy/mimori/tests/cli_mcp.rs#L769) and a memory leak—alongside CLI `--json` contract violations in the `memory` command and unplumbed `workspace_dir` parameters in multiple MCP tool handlers.

## 1. Correctness
- **CLI `--json` Contract Violation in `memory` Command** ([`src/main.rs:561-572, 594-604`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L561-L572)): In [`Commands::Memory`](file:///home/devhax/projects/fusuyfusuy/mimori/src/cli/args.rs#L203), checking `if let Some(s) = sec` precedes `else if cli.json`. Calling `mimori --json memory --section <name>` or `mimori --json memory show --section <name>` bypasses JSON serialization entirely, printing raw Markdown ([`print_budgeted`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L842)) or `MEM_EMPTY: ...` to stdout. This directly breaches Repository Invariant 2.
- **Unused `workspace_dir` in MCP Handlers** ([`src/mcp/tools.rs:432, 511, 558`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L432)): `mimori_slice`, `mimori_blast`, and `mimori_graph` declare `workspace_dir` in their MCP schemas and validate it via [`resolve_workspace_scope`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L401), but assign the result to an unused variable `_scope_dir`. All graph resolutions and line slicing default to `session.root`, silently ignoring subproject targeting.
- **Non-Standard Compact JSON in Graph Commands** ([`src/main.rs:113, 169, 208`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L113)): While `slice`, `map`, `blast`, `find`, `dump`, and `doctor` output pretty-printed JSON (`to_string_pretty`), `up`, `down`, and `uses` output compact unindented JSON, causing inconsistent formatting across CLI interfaces.
- **Unvalidated `jsonrpc` Version Header** ([`src/mcp/protocol.rs:6`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/protocol.rs#L6), [`src/mcp/mod.rs:118-234`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L118-L234)): [`JsonRpcRequest`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/protocol.rs#L5) does not enforce `jsonrpc == "2.0"`. Any version string (e.g. `"1.0"`) is accepted without generating JSON-RPC `-32600` (Invalid Request).
- **Missing JSON-RPC Batch Support** ([`src/mcp/mod.rs:63`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L63)): Stdio line parsing assumes a single object; JSON-RPC 2.0 batch request arrays `[...]` fail deserialization and yield `-32700` (Parse error).

## 2. Robustness & Process Lifecycle
- **Strict STDOUT Purity** ([`src/mcp/mod.rs:36`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L36), [`src/lib.rs:37`](file:///home/devhax/projects/fusuyfusuy/mimori/src/lib.rs#L37)): In MCP server mode, stdout is strictly reserved for JSON-RPC messages ([`send_response`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L236)). The startup banner and [`Phase`](file:///home/devhax/projects/fusuyfusuy/mimori/src/lib.rs#L29) timers are directed strictly to `eprintln!` (STDERR). Core library and workspace modules contain zero raw stdout `print!` calls.
- **Unbounded Memory Leak & Stale Drop in `cancelled_ids`** ([`src/mcp/mod.rs:46, 68, 98`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L46)): `cancelled_ids` stores cancelled request IDs in an `Arc<Mutex<HashSet<Value>>>`. IDs are removed *only* if matching incoming requests are dequeued after cancellation. When cancellation arrives after request execution completes, the ID is never removed. This leaks memory monotonically and silently drops future requests if the client reuses request IDs.
- **Clean Process Termination on EOF** ([`src/mcp/mod.rs:55, 73, 89`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L55)): When the client closes stdin, the background reader loop breaks, dropping `tx`. Channel closure terminates the `rx` loop, allowing clean server shutdown without lingering zombie threads.

## 3. Deep Analysis of `cli_mcp` Test Failure
- **Failure Site**: `tests/cli_mcp.rs:839` in [`test_mcp_whole_session_stdout_purity`](file:///home/devhax/projects/fusuyfusuy/mimori/tests/cli_mcp.rs#L769).
- **Observed Panics**: `assertion failed: left == right: left = String("call_cancel_5"), right = "ping_6"`.
- **Root Cause Analysis**:
  1. In step 5 of the test ([`tests/cli_mcp.rs:818-840`](file:///home/devhax/projects/fusuyfusuy/mimori/tests/cli_mcp.rs#L818-L840)), the test sends `call_cancel_5` (`tools/call`), `notifications/cancelled`, and `ping_6` back-to-back, then calls `client.recv()` expecting `ping_6`.
  2. The server uses two threads: a background Stdin Reader ([`src/mcp/mod.rs:50-87`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L50-L87)) and the Main Server Thread running the synchronous dispatch loop ([`src/mcp/mod.rs:89-113`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L89-L113)).
  3. The main thread pops `call_cancel_5` from `rx`. At that instant, the reader thread has not yet parsed line 2 (`notifications/cancelled`), so `cancelled_ids` is empty.
  4. The main thread executes `mimori_map` synchronously. Because the test workspace is tiny, [`handle_request`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L118) completes in <1ms.
  5. The post-execution cancellation check (`cancelled_ids.remove(id)`) executes before the reader thread inserts `call_cancel_5`. The server writes the response for `call_cancel_5` to stdout.
  6. The test client reads `call_cancel_5` instead of `ping_6`, failing the assertion.
- **Protocol vs Session Bug Assessment**:
  - *Protocol Specification*: MCP explicitly notes that cancellation is best-effort and asynchronous; clients cannot assume an in-flight or completed request is suppressed. The test makes an invalid assumption by expecting immediate synchronous suppression.
  - *Server Architecture Bug*: The server's cancellation model is flawed. Because tool execution is single-threaded and blocking without cooperative cancellation tokens, requests can never be aborted mid-execution. Moreover, late-arriving cancellation notices leak in `cancelled_ids` indefinitely.

## 4. Security & Path Confinement
- **Robust Path Confinement** ([`src/mcp/tools.rs:31-65, 401-419`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L31-L65)): The [`confine`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L31) utility canonicalizes candidate paths and validates that they start with the canonical workspace root. Relative directory traversal (`../`) and outside absolute paths (e.g. `/etc/passwd:#L1-5`) are strictly rejected with JSON-RPC error `-32602`. Symlink escapes outside the workspace boundary are rejected upon canonicalization.
- **Workspace Dir Traversal Guard** ([`src/mcp/tools.rs:408`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L408)): [`resolve_workspace_scope`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L401) enforces that `workspace_dir` is relative, preventing arbitrary absolute directory escapes.
- **State Mutation Confinement** ([`src/mcp/tools.rs:625-639, 663`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L625-L639)): Mutating operations in MCP (`mimori_memory resolve` and `mimori_debt sync`) resolve their filesystem target strictly inside the confined scope root, preventing unauthorized modification outside the workspace.

## Remediation Roadmap
1. **Fix Invariant 2 in `src/main.rs`**: In [`Commands::Memory`](file:///home/devhax/projects/fusuyfusuy/mimori/src/cli/args.rs#L203), prioritize `if cli.json` when `--section` is provided, returning `json!({ "section": s, "content": ... })` or `{ "error": "section not found" }`.
2. **Eliminate Cancellation Race & Leak**: In [`src/mcp/mod.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L46), pass cancellations through the FIFO channel or bound `cancelled_ids` with a ring-buffer TTL/size limit. In [`tests/cli_mcp.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/tests/cli_mcp.rs#L839), update the cancellation test assertion to tolerate best-effort response arrival or drain pending responses.
3. **Plumb `_scope_dir` in MCP Tools**: Pass the resolved `scope_dir` to `build_slice`, `generate_map`, and `calculate_blast_radius` instead of hardcoding `session.root`.
4. **Validate `jsonrpc == "2.0"`**: Reject requests where `jsonrpc != "2.0"` with error `-32600`.
