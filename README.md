# mimori

> **Fast, local AST code intelligence, blast-radius analysis, and project memory in Rust.**

[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)]()
[![Version](https://img.shields.io/badge/version-3.0.0-blue.svg)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **Status**: `mimori` is an experimental tool in active daily use across our repositories and agent harnesses. It evolves directly from real-world usage—when an agent hits a blind spot or a rough edge in production, we fix the parser or graph engine and ship it.

---

## Why mimori?

Reading whole files to find one function wastes context tokens; grepping for callers misses aliases, constructors, and transitive impact. `mimori` gives coding agents and developers a fast, deterministic view of a codebase without running background daemons:

- **Slice (`mimori slice`)**: Read a single symbol's body, signature, imports (`-i`), line numbers (`-n`), 1-hop callers/callees, and inlined private helpers (`-f`) in ~120 tokens instead of 3,000+.
- **Blast Radius (`mimori blast` & `mimori diff`)**: Trace upstream callers, affected entry points, and test suites before changing a signature—or run `mimori diff` to inspect the combined blast cone of your uncommitted `git diff` in one command.
- **Architectural Map (`mimori map` & `mimori dump`)**: Rank symbols and entry points via In-Degree PageRank. `mimori dump` packs the map, domain vocabulary, gotchas, and active debt into a single Turn-0 snapshot.
- **Project Memory & Debt Gate (`mimori memory` & `mimori debt`)**: Maintain a concise `.agents/memory.md` ledger and scan in-code `# mimodept: <what> <- <ceiling> -> <trigger>` comments with a hard 30-item ceiling.
- **Polyglot & Content-Hash Indexed**: Embedded Tree-sitter parsers for Rust, TypeScript/JavaScript, Python, and Go, backed by an incremental SQLite cache (`.mimori/index.db`) keyed on FNV-1a content hashes—never `mtime`.

---

## Installation

```bash
cargo install --git https://github.com/fusuyfusuy/mimori.git
```

Or from a local checkout:

```bash
git clone https://github.com/fusuyfusuy/mimori.git
cd mimori
cargo install --path .
```

---

## Usage

```shell
mimori dump    [--budget <N>] [--focus <target>] [--json]
mimori map     [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice   <coordinate> [-f|--follow-local] [-i|--with-imports] [-n|--numbered] [--budget <N>] [--json]
mimori find    <pattern> [-s|--symbols-only] [-f|--files-only] [--scope <dir>] [--limit <N>] [--json]
mimori blast   <target> [-d|--depth <N>] [--down] [--with-sinks <a,b,c>] [--json]
mimori diff    [--staged] [--base <rev>] [-d|--depth <N>] [--json]
mimori up      <target> [--json]
mimori down    <target> [--json]
mimori uses    <target> [--json]
mimori missing <pattern> [--scope <dir>] [--defines <lit>] [--json]
mimori doctor  [--limit <N>] [--json]
mimori memory  [show|lint|resolve] [--section <sec>] [--budget <N>] [--json]
mimori debt    [list|check|sync] [--scope <dir>] [--json]
mimori init    [--force]
mimori clean   [--all]
mimori mcp     [--workspace <dir>]
```

### Coordinates

`mimori` accepts standard `file:symbol` coordinates as well as Markdown-style `#` anchors:

```shell
src/auth.rs:authenticate          # symbol in a file
src/auth.rs#authenticate          # Markdown link style
src/service.ts:UserService::find  # method on a class/type
src/main.rs:#L20-45               # line range (also accepts #L20-L45 or :L20-45)
authenticate                      # bare symbol name across the workspace
```

Coordinates never guess silently. If a name is ambiguous across files, `mimori` exits non-zero and lists the ranked candidates; if a symbol is missing in a known file, it lists the symbols defined in that file.

### Common Workflows

```shell
# 1. Orient on a repository in one call
mimori dump --budget 1500

# 2. Slice a function with its imports, line numbers, and local helpers
mimori slice src/graph/blast.rs:calculate_blast_radius -f -i -n

# 3. Check upstream callers (with d=1 call-site line numbers) before an edit
mimori blast src/model/coordinate.rs:Coordinate::parse -d 3

# 4. Audit the blast radius and test impact of your current git diff
mimori diff

# 5. Reconcile and gate in-code # mimodept: debt markers
mimori debt sync
mimori memory lint && mimori debt check
```

---

## MCP Server

Run `mimori` over JSON-RPC stdio for AI coding agents:

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

Exposed tools:
- `mimori_dump` — Turn-0 context snapshot (`budget`, `focus`)
- `mimori_slice` — AST symbol or line-range slice (`coordinate`, `follow_local`, `with_imports`, `numbered`, `budget`)
- `mimori_blast` — Upstream/downstream transitive impact (`target`, `depth`, `down`, `with_sinks`)
- `mimori_diff` — Git diff symbol & blast-radius audit (`staged`, `base`, `depth`)
- `mimori_find` — PageRank-ordered symbol, file, and literal fallback search (`pattern`, `scope`, `symbols_only`, `files_only`, `limit`)
- `mimori_map` — Centrality-ranked structural outline (`scope`, `focus`, `seed`, `limit`)
- `mimori_graph` — Direct `up`, `down`, and `uses` traversal (`target`, `direction`)
- `mimori_memory` — `.agents/memory.md` reader, linter, and resolver (`action`, `section`, `target`, `budget`)
- `mimori_debt` — `# mimodept:` scanner, checker, and ledger sync (`action`, `scope`)

---

## Storage & Exit Codes

- **`.mimori/index.db`**: Ephemeral SQLite WAL cache. Safe to delete anytime via `mimori clean`; automatically rebuilt when `PARSER_VERSION` increments.
- **`.agents/memory.md`**: Git-tracked project memory and debt ledger.
- **`.mimoriignore`**: Optional ignore patterns supplementing `.gitignore`.

| Exit Code | Meaning |
| --------- | ------- |
| `0` | Success |
| `1` | Symbol not found, ambiguous coordinate, lint/debt check failure, or runtime error |
| `2` | Invalid CLI arguments |

---

## License

MIT © [Yusuf Akcakaya](LICENSE)
