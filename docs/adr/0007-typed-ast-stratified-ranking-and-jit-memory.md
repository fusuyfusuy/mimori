# 7. Typed AST Flags, Stratified Ranking, Hardened Confinement, and JIT Scoped Memory

- **Status**: Accepted (Amends ADR-0005 and ADR-0006)
- **Date**: 2026-11-23
- **Deciders**: Captain, Mimori Engineering

## Context

An end-to-end adversarial audit of `mimori` v3.1.1 revealed systemic accuracy, security, and incentive defects across all subsystems:

1. **Navigation Precision & Centrality Inversion**: Cross-file receiver calls (`db.load_all_symbols()`) were 100% dropped without warning while receiverless Tier-1 member calls bound stdlib `.is_empty()`/`.push()`/`.parse()` into unrelated local methods, and Tier-2/3 cross-file calls hijacked test helpers (e.g. `tests/cli_path_aliases.rs:write`). In-file Rust `#[cfg(test)]` symbols were invisible to path-based test heuristics, causing `doctor` to report an ~86% false-positive rate and `map`/`dump` to fill Top-25 rankings with private leaf helpers.
2. **Confinement & Storage Atomicity**: CLI absolute paths silently re-rooted the workspace outside `cwd`; `confine_to_workspace` leaked relative `../` existence oracles; manifest parsers followed escaping symlinks; `enforce_alias_fingerprint` + `save_batch` ran in separate transactions allowing concurrent readers to observe an empty index; and corrupt or read-only SQLite databases crashed queries.
3. **Memory Incentives & Scanner Noise**: Static Turn-0 memory dumps competed with code maps without section floors; `.agents/decisions.md` duplicated `docs/adr/`; a hand-rolled line scanner produced false positives on raw strings/docstrings while supporting legacy `ponytail:` comments; and a hard CI `exit 1` on >30 debt items triggered Goodhart's Law (incentivizing agents to delete valid debt records or silently truncate waivers during `debt sync`).

## Decision

We lock `mimori`'s North Star strictly to **in-repo navigation** and **in-repo agent memory** and execute the following architectural upgrades:

1. **Typed AST `SymbolFlags` & Local Receiver-Type Inference**:
   - Store a bitflag byte (`IS_TEST`, `IS_PUBLIC`, `IS_TRAIT_IMPL`, `IS_ATTR_ENTRY`) on every `Symbol` (SQLite `PARSER_VERSION = 15`), eliminating 97% test-detection blindness and `doctor` false positives on trait impls, `#[serde(...)]` hooks, and `#[cfg(test)]` modules.
   - Infer local variable receiver types across Rust, TS/JS, Go, and Python to emit qualified cross-file `Type::method` edges; block cross-file calls from production code into test or private targets; gate receiverless Tier-1 member calls against generic stdlib method names; and surface explicit `unverified_member_calls` warnings in `blast` whenever unresolved cross-file `.method()` call sites remain.
2. **Module-Stratified Public-API Map Ranking**:
   - Default `mimori map` to `--limit 30` (`--limit 0` for unlimited) so accidental unbounded calls never flood agent context.
   - Exclude test symbols from global unseeded/unfocused maps, elevate entry points and public API surfaces over private <=5 LOC zero-out-degree leaf helpers, and stratify Top-N selection round-robin across top-level module buckets so every core subsystem is guaranteed visibility.
3. **Hardened Confinement & Atomic Storage**:
   - Anchor CLI roots strictly to `cwd` or explicit `--workspace <dir>` (never untrusted coordinate paths); enforce lexical `..` normalization before `canonicalize()` across CLI, MCP, `--scope`, and manifest parsing.
   - Execute alias swaps, deletions, upserts, and symbol loads inside a single `BEGIN IMMEDIATE` transaction; auto-rebuild corrupt SQLite databases and fall back to in-memory parsing on read-only filesystems; use POSIX `ino+size+mtime+ctime` stat gating in `McpCache` with LRU eviction and session-root graph reuse for subdirectory `workspace_dir` calls.
4. **JIT Scope-Anchored Agent Memory & Tree-Sitter Debt Extraction**:
   - Support `@scope: <glob/prefix>` and `@sym: <symbol>` anchors in `.agents/memory.md`, automatically injected into `slice`, `blast`, and `diff` outputs when touching matching files or symbols.
   - Add `mimori memory add` (CLI & MCP) for safe, lock-guarded structured writes; index `docs/adr/*.md` titles in `mimori dump` (retiring redundant `.agents/decisions.md` scaffolding); and reserve Turn-0 budget floors (>=40% Map, <=40% Epics/Vocab/ADRs, <=25% Debt).
   - Extract `mimodept:` markers directly from Tree-sitter comment AST nodes (dropping legacy `ponytail:`), preserve all items without truncation on `debt sync`, and downgrade the 30-line debt ceiling to a non-blocking CI warning (`MEM_LINT_WARN` / `DEBT_CHECK_WARN`, `exit 0`) while keeping syntax/strikethrough/read-error checks hard-failing (`exit 1`).

## Consequences

- **Positive**: High-precision call graphs and `doctor` reports; honest `blast` warnings where static receiver typing is incomplete; byte-identical JSON & Markdown determinism (`BTreeMap` call counts); strict security boundaries; and memory notes delivered just-in-time at the point of edit rather than lost in static Turn-0 prose.
- **Trade-offs**: Breaking `SymbolKind` JSON serialization from PascalCase to lowercase (matching `as_str()` and `.d.ts` docs) and requiring `--workspace` or matching `cwd` for absolute CLI coordinates.
