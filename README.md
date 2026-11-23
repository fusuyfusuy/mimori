# mimori

> **Fast, local AST code intelligence, blast-radius analysis, and JIT in-repo agent memory in Rust.**

[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)]()
[![Version](https://img.shields.io/badge/version-3.2.0-blue.svg)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **Mission**: Give coding agents a faster, token-disciplined way to **learn the repo they are in** (`dump`, stratified `map`, `find`, `slice` + JIT in-repo memory) and **inspect blast radii & change impact** before/after edits (`blast`, `diff`, `up`/`down`/`uses`, `missing`, `doctor`)—zero background daemons, deterministic outputs, and strict workspace confinement.

---

## Why mimori?

Reading whole files to find one function wastes context tokens; grepping for callers misses aliases, constructors, receiver types, and transitive impact. `mimori` gives coding agents and developers a fast, deterministic view of a codebase:

- **Slice (`mimori slice`)**: Read a single symbol's body, signature, imports (`-i`), line numbers (`-n`), 1-hop callers/callees, inlined private helpers (`-f`), and **JIT scoped memory notes** (`@scope:`/`@sym:`) in ~150–400 tokens instead of 3,000+.
- **Blast Radius (`mimori blast` & `mimori diff`)**: Trace upstream callers, affected entry points, test suites, scoped memory, and explicit **unresolved member-call honesty warnings** before changing a signature—or run `mimori diff` to inspect the combined blast cone of your uncommitted `git diff` in one command.
- **Architectural Map (`mimori map` & `mimori dump`)**: Module-stratified, public-API-weighted PageRank orientation (defaults to top 30 symbols; `--limit 0` for all). `mimori dump` packs the map, `docs/adr/` titles, domain vocabulary, gotchas, and active debt into a budget-capped Turn-0 snapshot.
- **In-Repo Agent Memory & Debt (`mimori memory` & `mimori debt`)**: Maintain a git-tracked `.agents/memory.md` ledger (`show`, `add --scope/--sym`, `lint`, `resolve`) and scan in-code `# mimodept: <what> <- <ceiling> -> <trigger>` comments via Tree-sitter comment AST extraction (soft 30-item warning ceiling).
- **Polyglot & Content-Hash Indexed**: Embedded Tree-sitter parsers for Rust, TypeScript/JavaScript, Python, and Go, backed by an atomic SQLite cache (`.mimori/index.db`) governed by FNV-1a content hashes with automatic corruption recovery and read-only in-memory fallback.

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

Global flags available on all commands: `--json`, `-w | --workspace <dir>`.

```shell
mimori dump    [--budget <N>] [--focus <target>] [--json]
mimori map     [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice   <coordinate>... [-f|--follow-local] [-i|--with-imports] [-n|--numbered] [--budget <N>] [--json]
mimori find    <pattern> [-s|--symbols-only] [-f|--files-only] [--scope <dir>] [--limit <N>] [--json]
mimori blast   <target>... [-d|--depth <N>] [--down] [--with-sinks <a,b,c>] [--json]
mimori diff    [--staged] [--base <rev>] [-d|--depth <N>] [--json]
mimori up      <target> [--json]
mimori down    <target> [--json]
mimori uses    <target> [--json]
mimori missing <pattern> [--scope <dir>] [--defines <lit>] [--json]
mimori doctor  [--limit <N>] [--json]
mimori memory  [show|add|lint|check|resolve] [--section <sec>] [--scope <glob>] [--sym <name>] [--budget <N>] [--json]
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
src/main.rs:#L20-45               # line range (also accepts #L20-L45 or :L20-45; 1-based)
authenticate                      # bare symbol name across the workspace
```

Coordinates never guess silently and never escape the workspace root. If a name is ambiguous across files, `mimori` exits non-zero and lists ranked candidates; if a symbol is missing in a known file, it lists the symbols defined in that file.

### Common Workflows

```shell
# 1. Orient on a repository in one call (map + ADRs + vocab + gotchas + debt)
mimori dump --budget 1500

# 2. Record a scope-anchored gotcha auto-injected into future slice/blast/diff calls
mimori memory add --section gotchas --scope "src/storage/*" "Re-indexing is strictly FNV-1a content-hash driven."

# 3. Slice a function with its imports, line numbers, local helpers, and scoped memory
mimori slice src/graph/blast.rs:calculate_blast_radius -f -i -n

# 4. Check upstream callers (with d=1 call-site line numbers & member-call honesty check)
mimori blast src/model/coordinate.rs:Coordinate::parse -d 3

# 5. Audit the blast radius, scoped memory, and test impact of your current git diff
mimori diff

# 6. Reconcile and gate in-code # mimodept: debt markers
mimori debt sync
mimori memory lint && mimori debt check
```

---

## MCP Server

Run `mimori` over JSON-RPC 2.0 stdio (`Content-Length` framed or NDJSON) for AI coding agents:

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

Exposed tools (11 total):
- `mimori_dump` — Turn-0 context snapshot (`budget`, `focus`)
- `mimori_map` — Module-stratified centrality outline (`scope`, `focus`, `seed`, `limit`)
- `mimori_find` — PageRank-ordered symbol, file, and literal fallback search (`pattern`, `scope`, `symbols_only`, `files_only`, `limit`)
- `mimori_slice` — AST symbol or line-range slice with JIT scoped memory (`coordinate`, `follow_local`, `with_imports`, `numbered`, `budget`)
- `mimori_graph` — Strict `up`, `down`, and `uses` traversal (`target`, `direction`)
- `mimori_blast` — Upstream/downstream transitive impact with unresolved-member warnings (`target`, `depth`, `down`, `with_sinks`)
- `mimori_diff` — Git diff symbol, blast-radius, and scoped-memory audit (`staged`, `base`, `depth`)
- `mimori_missing` — Absence query for files lacking a literal pattern (`pattern`, `scope`, `defines`)
- `mimori_doctor` — Repository health, top hubs, and dead-weight audit (`limit`)
- `mimori_memory` — `.agents/memory.md` reader, structured writer (`add`), linter, and resolver (`action`, `section`, `text`, `scope`, `sym`, `target`, `all`, `budget`)
- `mimori_debt` — Tree-sitter comment `# mimodept:` scanner, checker, and ledger sync (`action`, `scope`)

---

## Storage & Exit Codes

- **`.mimori/index.db`**: Ephemeral SQLite WAL cache. Safe to delete anytime via `mimori clean`; auto-rebuilds on schema/version change or corruption, and falls back to in-memory parsing on read-only filesystems.
- **`.agents/memory.md`**: Git-tracked project memory, `@scope:`/`@sym:` JIT notes, and debt ledger.
- **`docs/adr/*.md`**: Architecture Decision Records automatically indexed by `mimori dump`.
- **`.mimoriignore`**: Optional ignore patterns supplementing `.gitignore`.

| Exit Code | Meaning |
| --------- | ------- |
| `0` | Success (including non-blocking soft debt ceiling warnings `> 30`) |
| `1` | Symbol not found, ambiguous coordinate, workspace escape, schema lint/debt syntax failure, or runtime error |
| `2` | Invalid CLI arguments |

---

## License

MIT © [Yusuf Akcakaya](LICENSE)
