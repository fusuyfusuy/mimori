---
scope: "memory-cli-mcp"
score: 9.2
status: "MINOR"
critical_findings: 0
invariant_breaches: []
---

# Scope 4 Verification Audit Report: Project Memory, CLI & MCP Protocol

Post-remediation verification audit of the project memory ledger, ponytail debt engine, MCP JSON-RPC 2.0 stdio server, and CLI command dispatch in `mimori`.

## 1. Executive Summary & Dimension Scores

| Dimension | Score | Assessment |
|---|:---:|---|
| **Correctness** | 9.3 | Substring resolve guard, 30-item ceiling enforcement, strict strikethrough ban, budget-bounded Turn-0 dump. |
| **Robustness** | 9.4 | Strict stdout purity, JSON-RPC 2.0 error framing, POSIX exit codes, atomic memory file writes. |
| **Performance** | 9.0 | Parallel Rayon debt scanner with 8-byte fast window check, FNV-1a fingerprint MCP cache. |
| **Security** | 9.2 | Dual-layer workspace confinement on `scope` and `workspace_dir`; zero shell command execution. |
| **Invariants** | 9.1 | Deterministic outputs, non-interactive CLI/MCP operations, zero persistent daemons. |

**Overall Health Score**: **9.2 / 10.0** (Status: **MINOR**)

---

## 2. Verification of Targeted Invariants

### 1. CLI Root Resolution in Subdirectories (`src/main.rs`)
- **Status**: **VERIFIED (Caller Alignment)** / **MINOR NOTE (Callee Behavior)**
- **Evidence**: [`src/main.rs#L298`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L298), [`#L411`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L411), [`#L434`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L434), [`#L484`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L484), [`#L520`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L520), [`#L669`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L669), [`#L740`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L740):
  Commands `Clean`, `Memory`, `Debt`, `Dump`, `Doctor`, `Map`, and `Init::scaffold` all invoke `find_workspace_root(None, &current_dir)`.
- **Architectural Nuance**: In [`src/workspace/walker.rs#L221-L224`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/walker.rs#L221-L224), `find_workspace_root` immediately returns `cwd.to_path_buf()` when `target_dir` is `None` (`let Some(target_dir) = target_dir else { return cwd.to_path_buf(); }`). As a result, upward marker traversal (`.mimori`, `.git`) only occurs if `target_dir` defaults to `cwd` (`target_dir.unwrap_or(cwd)`). Furthermore, `Commands::Find` ([`src/main.rs#L94`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L94)) and `Init` cache creation ([`src/main.rs#L451`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L451)) still pass `&current_dir`.

### 2. `mimori_debt` Scope Confinement (`src/mcp/tools.rs`, `src/memory/debt.rs`)
- **Status**: **VERIFIED**
- **Evidence**:
  - MCP Tool Boundary ([`src/mcp/tools.rs#L694-L697`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs#L694-L697)): Evaluates `confine(&scope_dir, Path::new(s))?`. Paths attempting to traverse outside the workspace root (e.g. `../../`) are rejected with `ToolError::InvalidParams("path escapes workspace: ...")`.
  - Domain Scanner ([`src/memory/debt.rs#L165-L178`](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/debt.rs#L165-L178)): Canonicalizes both `root` and `joined = root.join(s)`. If `!canon_joined.starts_with(&canon_root)`, it drops execution and returns `Vec::new()`.

### 3. `MemoryLedger::resolve` Empty Pattern Rejection (`src/memory/ledger.rs`)
- **Status**: **VERIFIED**
- **Evidence**: [`src/memory/ledger.rs#L290-L295`](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/ledger.rs#L290-L295):
  `let pattern_trimmed = pattern.trim(); if pattern_trimmed.is_empty() { anyhow::bail!("Cannot resolve debt item: pattern must not be empty"); }`.
  Empty and whitespace-only patterns fail immediately, preventing accidental catastrophic deletion of all active debt entries. CLI and MCP callers propagate non-zero exit codes and execution errors respectively.

### 4. Stdout Purity in MCP Mode (`src/mcp/mod.rs`, `src/mcp/protocol.rs`)
- **Status**: **VERIFIED**
- **Evidence**:
  - MCP stdio runner ([`src/mcp/mod.rs#L43-L44`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L43-L44), [`#L240-L246`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs#L240-L246)): `stdout` writes are restricted exclusively to `send_response(&mut stdout, &resp)`, emitting strictly valid newline-delimited JSON-RPC 2.0 messages.
  - Diagnostics, progress, and tracing across the entire codebase (`storage/sync.rs`, `workspace/aliases.rs`, `lib.rs`) route strictly to `eprintln!` (`stderr`).
  - Integration Test ([`tests/cli_mcp.rs#L821-L911`](file:///home/devhax/projects/fusuyfusuy/mimori/tests/cli_mcp.rs#L821-L911)): `test_mcp_whole_session_stdout_purity` validates that handshake, pings, tool calls, cancellations, unknown methods, and malformed inputs produce zero stdout pollution.

---

## 3. Remaining Minor & Hygiene Findings

1. **`walker::find_workspace_root` None-Target Early Return** ([`src/workspace/walker.rs#L222-L224`](file:///home/devhax/projects/fusuyfusuy/mimori/src/workspace/walker.rs#L222-L224)):
   `find_workspace_root` returns `cwd` without traversing parent markers when `target_dir` is `None`. Changing `let target_dir = target_dir.unwrap_or(cwd);` enables seamless repository root discovery from subdirectories for all CLI commands.
2. **`Commands::Find` & `Commands::Init` Subdirectory Passing** ([`src/main.rs#L94`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L94), [`#L451`](file:///home/devhax/projects/fusuyfusuy/mimori/src/main.rs#L451)):
   `Commands::Find` passes `&current_dir` to `execute_find` instead of `root`. `Commands::Init` creates `.mimori` and `.gitignore` at `current_dir` rather than the discovered root.
3. **Regex Help vs Substring Match Mismatch** ([`src/cli/args.rs#L240`](file:///home/devhax/projects/fusuyfusuy/mimori/src/cli/args.rs#L240)):
   `MemoryResolveArgs.pattern` help string specifies "Substring or regex pattern", but `ledger.resolve()` implements plain substring search (`line.contains(&pattern_lower)`).
4. **Silent Dropping of Invalid Markers in `sync`** ([`src/memory/debt.rs#L327`](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/debt.rs#L327)):
   `sync_debt` filters invalid markers (`m.valid_trigger`) without reporting their counts or locations in `DEBT_SYNC` summary output.

---

## 4. Invariant Compliance Matrix

| AGENTS.md Invariant | Status | Evidence / Analysis |
|---|---|---|
| **Deterministic & Non-Interactive** | **PASS** | Structured JSON (`--json`), deterministic exit codes, and standard JSON-RPC 2.0 schemas across all tools. |
| **Workspace Confinement** | **PASS** | Canonical prefix checks in `confine()` and `scan_debt_markers` ensure MCP and CLI operations never escape workspace root. |
| **Zero Background Daemons** | **PASS** | Synchronous lifecycle; MCP runs strictly over stdio during the agent session without background processes. |
| **Content-Hash Driven** | **PASS** | MCP symbol graph caching keys on FNV-1a workspace content hashes; memory files write atomically. |

---

## 5. Prioritized Actionable Recommendations

1. **Enable Upward Root Discovery on `None`** (P2 — `src/workspace/walker.rs`):
   Set `let start_dir = target_dir.unwrap_or(cwd);` and walk upwards for `.mimori` / `.git`.
2. **Align `Find` & `Init` Root Resolution** (P3 — `src/main.rs`):
   Pass the resolved `root` to `execute_find` and use `root` for `.mimori`/`.gitignore` scaffolding in `Commands::Init`.
3. **Report Invalid Marker Count during `sync`** (P3 — `src/memory/debt.rs`):
   Include invalid marker warning counts in `DEBT_SYNC` return message when `valid_markers.len() < markers.len()`.
