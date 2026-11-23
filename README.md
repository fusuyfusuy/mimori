# mimori

**Give coding agents a faster way to learn the repo they are in — and see the blast radius before they touch code.**

[![CI](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml/badge.svg)](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](Cargo.toml)
[![Version](https://img.shields.io/badge/version-3.2.0-blue.svg)](https://github.com/fusuyfusuy/mimori/releases/tag/v3.2.0)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

`mimori` is a single-binary, zero-daemon AST code-intelligence and in-repo agent memory engine written in Rust. It uses embedded Tree-sitter parsers (Rust, TypeScript/JavaScript/TSX/JSX, Python, Go) and an atomic local SQLite cache to answer two questions in ~20 ms — over the CLI or native MCP stdio:

| Job | Commands & MCP Tools | What It Gives an Agent |
| --- | --- | --- |
| **1. Learn the repo fast** | `dump`, `map`, `find`, `slice`, `memory` | Budget-capped Turn-0 snapshot (module-stratified map + `docs/adr/` titles + gotchas), hybrid search, and 1-hop AST symbol slices with **JIT `@scope:`/`@sym:` memory** auto-injected at edit time. |
| **2. Trace blast radii & change impact** | `blast`, `diff`, `up`, `down`, `uses`, `missing`, `doctor` | Transitive callers/callees, affected entry points and test suites, explicit **unresolved `.method()` honesty warnings**, absence sweeps, and combined blast analysis over your uncommitted `git diff`. |

---

## Why mimori?

Reading whole files to inspect one function burns context tokens; grepping for callers misses path aliases, constructors, receiver types, and multi-hop entry points; static repo docs go stale unless memory is surfaced where code is actually edited.

Measured on this repository (`51` source files, `843` symbols, `v3.2.0` release binary):

| Task | `mimori` | Baseline | Warm Latency |
| --- | --- | --- | --- |
| Read `confine_to_workspace` + imports + 1-hop callers + scoped gotcha | **~366 tok** (`slice -i -n`) | ~5,433 tok full file (**0.07×**) | ~21 ms |
| Read `SymbolGraph::resolve` + imports + 1-hop callers | **~713 tok** (`slice -i -n`) | ~16,667 tok full file (**0.04×**) | ~24 ms |
| Orient across core modules + ADRs + gotchas + debt | **~1,493 tok** (`dump --budget 1500`) | Manual tree + manifests + docs | ~22 ms |
| Upstream impact of `MemoryLedger::lint` (entry points + tests + warning) | **~422 tok** (`blast -d 2`) | Multi-hop manual `rg` | ~23 ms |

### End-to-End 3-Arm Agent Benchmark (`Dokploy` Monorepo)

Measured end-to-end on real [Dokploy Issue #6096](https://github.com/Dokploy/dokploy/issues/6096) (zero filenames in prompt; pinned at pre-fix commit `e193834bb`, `1,168` indexed TS/TSX/JS/JSX files, `11,732` symbols; `mimori` cold index `568 ms`, warm query `169 ms`). Three isolated agents ran the identical task on the same model (`gemini-4-neon-xhigh`, zero timeouts, verified `0` `mimori` calls in Arms B & C). All three arms produced the **exact same byte-for-byte upstream fix** (`879d5c74a`, `100/100` rubric), isolating pure efficiency:

| Metric | **Arm A (`mimori + rg`)** | **Arm B (`no-mimori surgical`)** | **Arm C (`default unsteered`)** | **A vs. B** | **A vs. C** |
| --- | ---: | ---: | ---: | ---: | ---: |
| **Upstream Fix & Rubric** | Exact (`100/100`) | Exact (`100/100`) | Exact (`100/100`) | Tied | Tied |
| **Time to First Edit** | **326 s** (5m 26s) | 407 s (6m 47s) | 424 s (7m 04s) | **−19.9%** | **−23.1%** |
| **Total Wall-Clock Time** | **477.8 s** (7m 58s) | 575.7 s (9m 36s) | 493.0 s (8m 13s) | **−17.0%** | **−3.1%** |
| **LLM Turns** | **18** | 18 | 22 | 0.0% | **−18.2%** |
| **Total Tool Calls** | 60 (`16` mimori, `9` read) | 64 (`0` mimori, `33` read) | 58 (`0` mimori, `31` read) | **−6.2%** | +3.4% |
| **`read_file` Calls** | **9** | 33 | 31 | **−72.7%** | **−71.0%** |
| **Distinct Files Opened** | **6** | 16 | 23 | **−62.5%** | **−73.9%** |
| **Raw Lines Read (`read_file`)** | **1,099** | 3,068 | 4,210 | **−64.2%** | **−73.9%** |
| **Uncached Input Tokens** | **130,697** | 157,462 | 157,290 | **−17.0%** | **−16.9%** |
| **Cache-Read Tokens** | 1,444,019 | 1,316,084 | 1,821,924 | +9.7% | **−20.7%** |
| **Output Tokens (incl. thinking)** | **48,205** | 58,024 | 54,142 | **−16.9%** | **−11.0%** |
| **↳ Thinking Tokens** | **26,622** | 37,847 | 30,395 | **−29.7%** | **−12.4%** |
| **Billable Tokens (`in + out`)** | **178,902** | 215,486 | 211,432 | **−17.0%** | **−15.4%** |
| **Peak Single-Turn Context** | **143,680** | 155,429 | 153,168 | **−7.6%** | **−6.2%** |
| **Est. API Cost** *(ref. rates)* | **$0.83** | $0.94 | $0.97 | **−12.3%** | **−14.5%** |

*Notes: Reference cost weighted at `$1.25/M` uncached input, `$0.125/M` cache-read, `$10/M` output. All three arms also ran ~13–14 custom Python scripts to exhaustively audit all 68 Drizzle schema tables for the issue's sibling-query deliverable; on the pure orientation + root-cause + caller blast-radius phase, Arm A reached the exact function, callers, and `#4256` precedent in **26 seconds (2 turns)**.*

---

## See It in Action

### 1. Slice a symbol (with JIT scoped memory + 1-hop graph)

```shell
mimori slice src/workspace/walker.rs:confine_to_workspace -i -n
```

```markdown
### Slice: `src/workspace/walker.rs:confine_to_workspace`

- **File**: `src/workspace/walker.rs`
- **Symbol**: `confine_to_workspace` (function)
- **Lines**: L535-L541
- **Signature**: `pub fn confine_to_workspace(workspace_root: &Path, candidate: &Path) -> Result<PathBuf>`
- **Scoped Memory**:
  - [@scope: src/workspace/*] Confinement invariant: Every CLI, MCP, manifest, and `--scope` path must pass lexical normalization + `confine_path` before touching disk (eliminates symlink escapes and `../` existence oracles).
- **1-Hop Callers**:
  - `src/graph/blast.rs:FileLineCache::with_symbol_lines (L225-L262)`
  - `src/graph/blast.rs:sweep_literal_sinks (L760-L804)`
  - `src/graph/mod.rs:SymbolGraph::read_symbol_body (L982-L1005)`
  - `src/graph/mod.rs:SymbolGraph::build_slice (L1007-L1108)`
  - `src/main.rs:confine_and_normalize_coord (L96-L138)`
  - _5 of 17 callers shown (use mimori up for full list)_
- **1-Hop Callees**:
  - `src/workspace/walker.rs:confine_path (L487-L533)`

L535: pub fn confine_to_workspace(workspace_root: &Path, candidate: &Path) -> Result<PathBuf> {
L536:     confine_path(
L537:         workspace_root,
L538:         candidate,
L539:         SymlinkPolicy::AllowWithinWorkspace,
L540:     )
L541: }
```

### 2. Trace blast radius before changing a signature

```shell
mimori blast MemoryLedger::lint -d 2
```

```markdown
# Blast Radius (Upstream): `MemoryLedger::lint` (Depth: 2, Affected: 3)

> Warning: 3 unresolved cross-file `.lint()` call site(s) exist in workspace (untyped receiver calls are not linked statically); verify with: `rg -n "\.lint\("`

### Scoped Memory
- [@scope: src/memory/*] Memory substrate invariant: `.mimori/` is ephemeral cache (gitignored); `.agents/memory.md` is git-tracked domain memory with soft 30-line debt ceiling (warn-only in CI) and Tree-sitter AST comment debt extraction.

### Affected Entry Points
- `src/main.rs:main`

### Affected Test Suites
- `src/memory/ledger.rs`

### Transitive Call Tree
  - (d=1) **`cmd_memory`** (function) -> `src/main.rs:cmd_memory` [calls @ L740]
  - (d=1) **`lint_allows_brackets_inside_debt_prose_and_checks_word_boundary_accepted`** (function) -> `src/memory/ledger.rs:...` [calls @ L1170] [Test]
    - (d=2) **`main`** (function) -> `src/main.rs:main` [Entry Point]
```

Notice the warning banner: when a method call crosses files through an untyped receiver, `mimori` **refuses to guess globally** and tells you exactly how many unresolved `.lint()` call sites exist plus the exact `rg` check to verify them.

---

## The 5-Step Agent Loop

```shell
# 1. Orient on Turn-0 (stratified map + docs/adr/ titles + gotchas + active debt)
mimori dump --budget 1500

# 2. Locate target symbols, files, or fallback literals (supports `|` batching)
mimori find "SymbolGraph|Coordinate::parse"

# 3. Inspect target code with imports, line numbers, local helpers, and JIT memory
mimori slice src/graph/mod.rs:SymbolGraph::resolve -f -i -n

# 4. Check upstream impact before editing a signature or contract
mimori blast src/model/coordinate.rs:Coordinate::parse -d 3

# 5. Audit your working-tree diff (mutated symbols, blast cone, tests, memory/debt gate)
mimori diff
```

Capture hard-won repo invariants once so every future `slice`, `blast`, and `diff` in that directory automatically surfaces them:

```shell
mimori memory add --section gotchas --scope "src/storage/*" \
  "Re-indexing is strictly FNV-1a content-hash driven."
```

---

## Installation

```bash
cargo install --git https://github.com/fusuyfusuy/mimori.git
```

Or build from a local checkout:

```bash
git clone https://github.com/fusuyfusuy/mimori.git
cd mimori
cargo install --path .
```

Agent skill instructions live in [SKILL.md](SKILL.md) (mirrored byte-identically at [skills/mimori/SKILL.md](skills/mimori/SKILL.md)).

---

## MCP Server

Run `mimori` over JSON-RPC 2.0 stdio (`Content-Length` framed or NDJSON). Warm MCP sessions hold an LRU in-memory graph cache gated by POSIX `ino+size+mtime+ctime` stat fingerprints so repeated calls avoid disk re-hashing.

```json
{
  "mcpServers": {
    "mimori": {
      "command": "mimori",
      "args": ["mcp"]
    }
  }
}
```

### Exposed MCP Tools (11)

| Tool | Purpose | Key Parameters |
| --- | --- | --- |
| `mimori_dump` | Turn-0 packed context (map + ADRs + vocab/gotchas + debt) | `budget`, `focus`, `workspace_dir` |
| `mimori_map` | Module-stratified, public-API-weighted PageRank outline | `limit` (default `30`, `0` = all), `scope`, `focus`, `seed`, `workspace_dir` |
| `mimori_find` | PageRank-ordered symbol, file, and literal fallback search (`\|` batching) | `pattern`, `scope`, `symbols_only`, `files_only`, `limit`, `workspace_dir` |
| `mimori_slice` | AST symbol or line-range slice with JIT `@scope:`/`@sym:` memory | `coordinate` (`string \| string[]`), `follow_local`, `with_imports`, `numbered`, `budget`, `workspace_dir` |
| `mimori_blast` | Upstream/downstream transitive blast radius + honesty warnings | `target` (`string \| string[]`), `depth`, `down`, `with_sinks`, `workspace_dir` |
| `mimori_diff` | Mutated symbols, combined blast cone, tests, and debt/memory gate | `staged`, `base`, `depth`, `workspace_dir` |
| `mimori_graph` | Strict 1-hop `up` (callers), `down` (callees), or `uses` (mentioners) | `target` (`string \| string[]`), `direction`, `workspace_dir` |
| `mimori_missing` | Absence query: files under `scope` lacking every `\|` alternative | `pattern`, `scope`, `defines`, `workspace_dir` |
| `mimori_doctor` | Index health, resolution metrics, top hubs, dead-weight candidates | `limit`, `workspace_dir` |
| `mimori_memory` | Read, `add` scoped notes, `lint`, or safely `resolve` `.agents/memory.md` | `action`, `section`, `text`, `scope`, `sym`, `target`, `all`, `budget`, `workspace_dir` |
| `mimori_debt` | Tree-sitter comment AST `# mimodept:` `list`, `check`, or `sync` | `action`, `scope`, `workspace_dir` |

---

## CLI Reference

Global flags on every command: `--json`, `-w | --workspace <dir>`.

### Repo Orientation
```shell
mimori dump    [--budget <N>] [--focus <target>]
mimori map     [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>]
mimori find    <pattern> [-s|--symbols-only] [-f|--files-only] [--scope <dir>] [--limit <N>]
mimori slice   <coordinate>... [-f|--follow-local] [-i|--with-imports] [-n|--numbered] [--budget <N>]
```

### Blast Radius & Change Impact
```shell
mimori blast   <target>... [-d|--depth <N>] [--down] [--with-sinks <a,b,c>]
mimori diff    [--staged] [--base <rev>] [-d|--depth <N>]
mimori up      <target>
mimori down    <target>
mimori uses    <target>
mimori missing <pattern> [--scope <dir>] [--defines <lit>]
mimori doctor  [--limit <N>]
```

### In-Repo Memory, Debt & Maintenance
```shell
mimori memory  [show|add|lint|check|resolve] [--section <sec>] [--scope <glob>] [--sym <name>] [--all] [--budget <N>]
mimori debt    [list|check|sync] [--scope <dir>]
mimori init    [--force]
mimori clean   [--all]
mimori mcp
```

### Coordinates

`mimori` accepts `file:symbol`, Markdown-style `file#symbol`, qualified `Type::method`, 1-based line ranges, and bare symbol names:

```shell
src/auth.rs:authenticate          # symbol in a file
src/auth.rs#authenticate          # Markdown anchor form
src/service.ts:UserService::find  # method on a type/class
src/main.rs:#L20-45               # 1-based line range (also #L20-L45 or :L20-45)
authenticate                      # bare symbol across the workspace
```

Ambiguous symbols never guess silently — `mimori` exits `1` with ranked candidates. Missing symbols inside a known file list the symbols actually defined in that file.

---

## Design Invariants & Honest Limits

- **Supported languages**: Rust (`.rs`), TypeScript (`.ts`, `.mts`, `.cts`), TSX/JSX (`.tsx`, `.jsx`), JavaScript (`.js`, `.mjs`, `.cjs`), Python (`.py`), and Go (`.go`), with monorepo alias resolution (`tsconfig.json`, `package.json`, `pnpm-workspace.yaml`, `Cargo.toml`, `go.mod`, `pyproject.toml`).
- **Static AST precision, not full typechecking**: Local receiver types and imports are resolved from ASTs; untyped cross-file `.method()` calls are tracked explicitly and surfaced as `blast` warnings rather than silently linked or silently omitted.
- **Content-hash indexed, zero daemons**: `.mimori/index.db` uses atomic SQLite WAL transactions keyed on FNV-1a content hashes (`PARSER_VERSION` bumps auto-rebuild). Corrupt caches rebuild automatically; read-only workspaces fall back to in-memory indexing.
- **Strict workspace confinement**: CLI coordinates, MCP `workspace_dir`, `--scope` filters, manifest discovery, and `clean --all` enforce lexical normalization and symlink confinement — no outside-root reads, writes, or `../` existence oracles.
- **Deterministic M2M output**: All `--json` and Markdown outputs are deterministically sorted (byte-identical across runs) and free of decorative noise.

---

## Storage Layout, Memory & ADRs

- **`.mimori/index.db`** — Ephemeral SQLite WAL cache (gitignored). Safe to delete anytime via `mimori clean [--all]`.
- **`.agents/memory.md`** — Git-tracked domain memory: active epics, domain vocabulary, `@scope:`/`@sym:` JIT gotchas, and open debt items synced from in-code `# mimodept: <what> <- <ceiling> -> <trigger>` comments (soft 30-item warning ceiling, never truncates waivers).
- **`docs/adr/*.md`** — Architecture Decision Records automatically title-indexed by `mimori dump`:
  - [ADR-0001](docs/adr/0001-rust-tree-sitter-architecture.md) — Rust CLI with Embedded Tree-sitter Grammars
  - [ADR-0002](docs/adr/0002-syntactic-graph-with-binary-cache.md) — Syntactic Graph with SQLite Persistence
  - [ADR-0003](docs/adr/0003-pagerank-in-degree-centrality.md) — In-Degree PageRank for Symbol Centrality
  - [ADR-0004](docs/adr/0004-pure-code-dissection-engine.md) — Pure Code Dissection Engine
  - [ADR-0005](docs/adr/0005-mcp-server-interface.md) — Model Context Protocol (MCP) Server Interface
  - [ADR-0006](docs/adr/0006-reclaiming-project-memory-substrate.md) — Reclaiming Project Memory Substrate
  - [ADR-0007](docs/adr/0007-typed-ast-stratified-ranking-and-jit-memory.md) — Typed AST Flags, Stratified Ranking, Hardened Confinement, and JIT Scoped Memory
- **`.mimoriignore`** — Optional ignore rules supplementing `.gitignore`.

### Exit Codes

| Exit Code | Meaning |
| --- | --- |
| `0` | Success (including non-blocking soft debt warnings `> 30` items) |
| `1` | Symbol not found, ambiguous coordinate, workspace escape, schema/debt syntax error, or runtime error |
| `2` | Invalid CLI arguments |

---

## License

MIT © [Yusuf Akcakaya](LICENSE)
