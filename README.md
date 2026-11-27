# mimori

**Give coding agents a faster way to learn the repo they are in — and see the blast radius before they touch code.**

[![CI](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml/badge.svg)](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](Cargo.toml)
[![Version](https://img.shields.io/badge/version-3.2.1-blue.svg)](https://github.com/fusuyfusuy/mimori/releases/tag/v3.2.1)
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

### End-to-End 3-Arm Agent Benchmark (`Dokploy` Monorepo, N = 3 Real Issues)

Measured end-to-end across **3 real Dokploy issues** (`9` independent full-agent runs, zero filenames in prompts, pinned to each issue's pre-fix commit with future history stripped; `1,168` indexed TS/TSX/JS/JSX files, `11,732` symbols; `mimori v3.2.0` cold index `568 ms`, warm query `169 ms`). All 9 runs used the identical model (`gemini-4-neon-xhigh`), ran with zero timeouts (`--print-timeout 0s`), had verified **`0` `mimori` calls in Arms B & C**, and achieved **equivalent, upstream-matched fixes** on every issue (`9/9` `SUCCESS`), isolating pure efficiency:

#### 3-Run Aggregate (`#6096` + `#5781` + `#6100`)

| Metric | **Arm A (`mimori + rg`)** | **Arm B (`no-mimori surgical`)** | **Arm C (`default unsteered`)** | **A vs. B** | **A vs. C** |
| --- | ---: | ---: | ---: | ---: | ---: |
| **Tasks Solved / Clean Swaps** | **3 / 3** (`9/9` matched) | 3 / 3 | 3 / 3 | Tied | Tied |
| **Total Wall-Clock Time** | **1,191.9 s** (19m 52s) | 1,383.3 s (23m 03s) | 1,542.5 s (25m 43s) | **−13.8%** | **−22.7%** |
| **Total Time to First Edit** | **679.0 s** (226s avg) | 967.0 s (322s avg) | 1,020.0 s (340s avg) | **−29.8%** | **−33.4%** |
| **Total LLM Turns** | **49** (16.3 avg) | 60 (20.0 avg) | 70 (23.3 avg) | **−18.3%** | **−30.0%** |
| **Total Tool Calls** | **158** (`52` mimori, `34` read) | 177 (`0` mimori, `101` read) | 198 (`0` mimori, `117` read) | **−10.7%** | **−20.2%** |
| **`read_file` Calls** | **34** | 101 | 117 | **−66.3%** | **−70.9%** |
| **Files Opened (`read_file`)** | **27** | 66 | 81 | **−59.1%** | **−66.7%** |
| **Raw Lines Read (`read_file`)** | **3,808** | 10,413 | 13,895 | **−63.4%** | **−72.6%** |
| **Uncached Input Tokens** | **374,626** | 506,920 | 493,614 | **−26.1%** | **−24.1%** |
| **Cache-Read Tokens** | **3,637,318** | 4,310,753 | 6,225,090 | **−15.6%** | **−41.6%** |
| **Output Tokens (incl. thinking)** | **117,327** | 136,864 | 164,106 | **−14.3%** | **−28.5%** |
| **↳ Thinking Tokens** | **61,124** | 83,448 | 84,607 | **−26.8%** | **−27.8%** |
| **Billable Tokens (`in + out`)** | **491,953** | 643,784 | 657,720 | **−23.6%** | **−25.2%** |
| **All-In Tokens (`in + cache + out`)** | **4,129,271** | 4,954,537 | 6,882,810 | **−16.7%** | **−40.0%** |
| **Mean Peak Context** | **132,484** | 137,085 | 159,628 | **−3.4%** | **−17.0%** |
| **Total Est. API Cost** *(ref. rates)* | **$2.10** | $2.54 | $3.04 | **−17.5%** | **−31.0%** |

#### Per-Issue Summary (Arm A Won 3/3 on Time, Time-to-Edit, Billable Tokens, Lines Read & Cost)

| Run / Issue | Archetype | Wall Time (`A / B / C`) | Billable `in+out` (`A / B / C`) | Raw Lines Read (`A / B / C`) | Est. Cost (`A / B / C`) |
| --- | --- | --- | --- | --- | --- |
| **#6096** (`e193834bb`) | Webhook + Drizzle 100-arg + schema audit | **478s** / 576s / 493s | **179k** / 215k / 211k | **1,099** / 3,068 / 4,210 | **$0.83** / $0.94 / $0.97 |
| **#5781** (`3b13bcde8`) | Multi-router RBAC across 7 `*.search` APIs | **251s** / 329s / 437s | **139k** / 164k / 225k | **1,050** / 3,976 / 5,849 | **$0.48** / $0.64 / $0.97 |
| **#6100** (`5ccc51a9c`) | Traefik `dynamic/` reload (local + remote certs) | **463s** / 479s / 613s | **174k** / 264k / 221k | **1,659** / 3,369 / 3,836 | **$0.79** / $0.96 / $1.10 |

*Notes: Reference cost weighted at `$1.25/M` uncached input, `$0.125/M` cache-read, `$10/M` output. Notice the largest wins occur on multi-file navigation & consistency tasks (`#5781`: **−42% wall time**, **−51% cost**, **−82% lines read** vs. default Arm C).*

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

Run `mimori` over JSON-RPC 2.0 stdio (accepts `Content-Length` framed or NDJSON requests; emits NDJSON responses). Warm MCP sessions hold an LRU in-memory graph cache gated by POSIX `ino+size+mtime+ctime` stat fingerprints so repeated calls avoid disk re-hashing.

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

- **Supported languages**: Rust (`.rs`), TypeScript (`.ts`, `.mts`, `.cts`), TSX/JSX (`.tsx`, `.jsx`), JavaScript (`.js`, `.mjs`, `.cjs`), Python (`.py`), and Go (`.go`), with monorepo alias resolution (`tsconfig.json`, `package.json`, `pnpm-workspace.yaml`, `Cargo.toml`, `go.mod`).
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
