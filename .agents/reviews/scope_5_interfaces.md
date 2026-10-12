---
scope: "cli-mcp-interfaces"
score: 8.0
status: "MODERATE"
critical_findings: 0
invariant_breaches: []
---
# CLI & MCP (src/main.rs, src/mcp) — sequential pass 2026-10-12

## I1 (Moderate) Unbounded stdin line in MCP server -> memory DoS
- src/mcp/mod.rs:59 `stdin.lock().lines()` buffers an entire line; unbounded `mpsc::channel()` at :52.
- Repro: 200 MB single line without newline -> `mimori mcp` max RSS 202,752 KB, exit 0. Scales linearly with input.
- Remediation: bounded line reader (e.g. 4 MiB cap -> -32600), `sync_channel` with fixed capacity.

## I2 (Minor) MCP surface omits `uses`, `missing`, `doctor`, `up`/`down` aliases
- tools/list exposes: slice, dump, map, find, blast, graph, memory, debt. CLI has uses, missing, doctor, up, down (`mimori --help`).
- The boundary-review protocol relies on `uses`/`missing`; MCP-only agents cannot run them.

## I3 (Minor) JSON-RPC: `id: null` parse error shape OK; `unwrap()` on mutex at mod.rs:75,109,116 poisons -> abort on panic in reader thread. Low likelihood.

## Falsification attempts that found nothing
- Path traversal: MCP slice `../../../etc/passwd:#L1-3` -> -32602 "path escapes workspace"; CLI same, exit 1.
- `mimori_find scope:"/etc"`: returned only workspace results (arg ignored; no escape).
- Garbage line -> -32700 id null; unknown method -> -32601. Server survives.
- `mimori_map limit=999999999`: bounded by symbol count.
