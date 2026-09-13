# mimori

> **Zero-config AST code-intelligence, symbol-graph, and context-slicing engine in Rust.**

[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

---

## What is mimori?

`mimori` gives AI coding agents and developers token-dense structural code intelligence and spatial awareness without background daemons:

- **Project Memory Substrate & Proof Gate**: Dedicated `.agents/` substrate (`memory.md` & `decisions.md`) tracking epics, active technical debt, domain vocabulary, and gotchas. `mimori memory lint` validates the 30-line debt ceiling and rejects completed checkboxes/strikethroughs in CI. `mimori memory resolve <pattern>` surgically purges resolved items.
- **In-Code Ponytail Technical Debt Engine**: Multi-threaded Rayon scanner detecting `# ponytail: <what> <- <ceiling> -> <trigger>` markers across codebases. `mimori debt list` surfaces markers, `mimori debt sync` reconciles them into `.agents/memory.md` while preserving operator waivers (`- accepted ...`), and `mimori debt check` validates ceilings and triggers as a CI proof gate.
- **Turn-0 Context Dumping**: `mimori dump` builds budget-aware prompt snapshots packing domain vocabulary, gotchas, active debt, and PageRank architectural entry points for zero-turn agent orientation.
- **Polyglot Embedded Tree-sitter AST**: Statically embeds Tree-sitter parsers for Rust, TypeScript/JavaScript, Python, and Go. Indexes functions, methods, classes, class fields (incl. constructor param properties), traits, interfaces, exported constants, builder patterns, and object literal members (e.g. tRPC routers, Drizzle tables, Hono routes). Edges cover calls, construction (`new X()` incl. `throw new X`, Rust `S::new()`/`S::default()`), member-access reads, call-arg mentions, and template-interpolation mentions.
- **In-Degree PageRank Centrality**: Ranks architectural entry points, hubs, and core data abstractions using power iteration (*d*=0.85, 25 iterations). Pass `--seed <term>` to bias the ranking toward matching symbols, or `--focus <symbol>` for Personalized PageRank around a specific component.
- **Context-Aware Slicing**: Extract isolated code slices containing exact source coordinates, 1-hop callers/callees, inlined private local callees (`-f`), top-of-file imports (`-i`), and line numbering (`-n`). Consumes ~120 tokens vs 3,000+ for raw whole-file reads.
- **Unambiguous Coordinates**: A coordinate resolves to exactly one symbol, or the command fails and lists the candidates. `mimori` never silently picks between two files that share a basename.
- **Transitive Blast Radius**: Evaluate ripple impact up to depth *N* before editing (`blast`), reporting affected callers, entry points, and test suites. Pass `--down` for the downstream cone (everything the target pulls in) — handy for delete-safety. Pass `--with-sinks <a,b,c>` to sweep literal sink substrings (`console.*`, `logPath`, …) the graph can't see, appended as per-line hits.
- **Repo Doctor**: `doctor` reports files/symbols/edges, top hubs by fan-in, and dead-weight candidates split into `likely dead` (isolated functions) vs `needs human review` (isolated types/constants).
- **Hybrid Search Fallback**: Fast symbol and file search (`find`). When zero symbols or files match, falls back to a case-insensitive literal scan of indexed files — one hit per matching line (count matches `rg -c` on the same query).
- **Construction-Aware Callers**: `up` on a class folds in `new X(...)` construction sites via its constructor. Zero callers on a class-like prints a `try X::constructor` / `rg "new X"` hint instead of leaving you to guess.
- **Zero-Daemon SQLite Cache**: Persistent incremental index in `.mimori/index.db` (WAL mode). Re-parsing is decided by FNV-1a **content hash**, so an edit that preserves the file's mtime cannot leave the index stale.

---

## Installation

### From Source

```bash
cargo install --git https://github.com/fusuyfusuy/mimori.git
```

Or clone and build:

```bash
git clone https://github.com/fusuyfusuy/mimori.git
cd mimori
cargo install --path .
```

---

## Usage

### Overview

```shell
mimori init    [--force]
mimori map     [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice   <coordinate> [-f|--follow-local] [-i|--with-imports] [-n|--numbered] [--budget <N>] [--json]
mimori find    <pattern> [-s|--symbols-only] [-f|--files-only] [--limit <N>] [--json]
mimori up      <target> [--json]
mimori down    <target> [--json]
mimori uses    <target> [--json]
mimori missing <pattern> [--scope <dir>] [--defines <lit>] [--json]
mimori blast   <target> [-d|--depth <N>] [--down] [--with-sinks <a,b,c>] [--json]
mimori doctor  [--limit <N>] [--json]
mimori memory  [show|lint|resolve] [--section <sec>] [--budget <N>] [--json]
mimori debt    [list|check|sync] [--scope <dir>] [--json]
mimori dump    [--budget <N>] [--focus <target>] [--json]
mimori clean   [--all]
mimori mcp     [--workspace <dir>]
```

### Coordinates

```shell
src/auth.rs:authenticate          # symbol in a file
src/service.ts:UserService::findUser
src/main.rs:#L20-45               # line range (read straight off disk, no index)
authenticate                      # bare name, resolved across the workspace
```

A file coordinate is matched by exact path first, then by a path suffix on a component
boundary, then by basename. **Whenever a tier matches more than one symbol, the command
exits non-zero and prints the candidates** rather than guessing:

```shell
$ mimori slice mod.rs:handler
Error: Ambiguous symbol 'mod.rs:handler'. Multiple matches found, please specify full coordinate:
  - `src/alpha/mod.rs:handler` (function) [rank: 0.0142]
  - `src/beta/mod.rs:handler` (function) [rank: 0.0138]
```

### Examples

#### 1. Initialize cache & inspect architectural map
```shell
mimori init
mimori map --limit 20
```

#### 2. Search for high-centrality symbols or literals
```shell
mimori find "authenticate" -s
mimori find "create-backup"
```

#### 3. Extract 1-hop AST context slice with imports & private helpers
```shell
mimori slice src/auth/service.rs:authenticate -f -i -n
```

#### 4. Evaluate blast radius before refactoring
```shell
mimori blast src/db/connection.rs:query -d 3
```

#### 5. Manage project memory, technical debt & Turn-0 context
```shell
mimori dump --budget 1500                          # Turn-0 agent orientation snapshot
mimori debt list                                   # scan in-code # ponytail: markers
mimori debt sync                                   # merge markers into .agents/memory.md
mimori debt check                                  # CI proof gate (triggers + 30-line ceiling)
mimori memory lint                                 # validate .agents/memory.md schema
mimori memory resolve "cache bypass"               # surgically remove resolved debt item
```

#### 6. Model Context Protocol (MCP) Server for Agents

Run `mimori` as a native JSON-RPC stdio daemon for AI coding agents:

```shell
mimori mcp [--workspace /path/to/repo]
```

Add to your MCP client configuration:

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

Exposes 7 high-leverage tools directly to LLMs with warm in-memory caching:
- `mimori_slice`: AST slice with signature, body, line numbering (`numbered: true`), and 1-hop callers/callees.
- `mimori_map`: Centrality-ranked PageRank codebase outline.
- `mimori_find`: Fast PageRank-ordered symbol and file search (`limit` defaults to 50).
- `mimori_blast`: Upstream/downstream impact analysis with literal sink detection.
- `mimori_graph`: Unified caller (`up`), callee (`down`), and mentioner (`uses`) traversal.
- `mimori_memory`: Read (`show`), validate (`lint`), and surgically resolve (`resolve`) project memory (`.agents/memory.md`).
- `mimori_debt`: Scan in-code markers (`list`), enforce CI gate (`check`), and reconcile (`sync`) ponytail technical debt.

All tools enforce strict workspace confinement: `workspace_dir` is interpreted relative to the session root, and absolute paths escaping the workspace are rejected.

---

## Performance

Indexing is near-linear in workspace size. Measured on a 1200-file / 38MB / 481,200-symbol
corpus (4 cores):

| Workspace | Cold (full parse) | Warm (no changes) |
| --------- | ----------------- | ----------------- |
| this repo (~4k lines) | 0.05s | &lt;0.01s |
| 120,300 symbols | 2.1s | 0.45s |
| 481,200 symbols | 7.9s | 1.75s |

Every command reads and hashes every source file so the index cannot go stale; that costs
about 0.4% of a warm run. Only files whose hash changed are re-parsed.

Set `MIMORI_PROFILE=1` to print per-phase timings to stderr:

```shell
$ MIMORI_PROFILE=1 mimori map >/dev/null
  [profile] scan+read+hash        95.2ms
  [profile] load_all_symbols     553.4ms
  [profile]   edge resolve       291.2ms
  [profile]   pagerank           203.6ms
```

---

## Files

| Path | Purpose |
| ---- | ------- |
| `.mimori/index.db` | SQLite index of files, symbols, and references (WAL). |
| `.mimoriignore` | Optional extra ignore patterns, supplementing `.gitignore`. |

The index is derived data and is safe to delete at any time; `mimori clean` does it for you.
It rebuilds automatically whenever the embedded parser version changes.

---

## Exit status

| Code | Meaning |
| ---- | ------- |
| `0` | Success. |
| `1` | Symbol not found, ambiguous coordinate, unreadable workspace, or database error. |
| `2` | Invalid command-line arguments. |

---

## License

MIT © [Yusuf Akcakaya](LICENSE)
