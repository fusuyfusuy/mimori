---
name: mimori
description: >
  Zero-config AST code-intelligence, symbol-graph, context-slicing, and project memory engine.
  Use for symbol search, 1-hop AST slicing, PageRank architectural mapping, blast-radius analysis,
  Turn-0 context dumping, and project memory/ponytail debt tracking. Native CLI and MCP stdio.
---

# MIMORI(1) — AST Intelligence & Memory Substrate

```text
KERNEL:
  TARGET: Autonomous Agent Execution & Code Intelligence Substrate
  BINARY: ~/.local/bin/mimori (CLI) | mimori mcp (JSON-RPC stdio)
  VERSION: 2.4.0
  INVARIANTS:
    1_HASH:       Content-Hash Governed (FNV-1a) — mtime purely untrusted
    2_PURITY:     Deterministic & Non-Interactive — zero background daemons
    3_CONFINED:   Workspace Confinement strictly enforced (Reject ../ escapes)
    4_STORAGE:    .mimori/ (ephemeral index cache) | .agents/ (git-tracked domain memory)
```

## SYNOPSIS

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

---

## AGENT LIFECYCLE PIPELINE

```text
CANOPY -> SLICE -> BLAST -> MUTATE -> PROVE -> RECONCILE
```

1. **TURN-0 (Orientation)**:
   - `mimori dump --budget 1500` | MCP: `mimori_memory(action="show")`
   - Injects domain vocabulary, gotchas, active debt, and PageRank entry points.
2. **CANOPY (Topography)**:
   - `mimori map --limit 25` | MCP: `mimori_map(limit=25)`
   - Identifies high-centrality symbols and entry points without reading raw files.
3. **SLICE (Targeted Reading)**:
   - `mimori slice <coord> -f -i -n` | MCP: `mimori_slice(coordinate=C, follow_local=true)`
   - Emits exact declaration body with 1-hop callers, callees, and inlined private helpers. Consumes ~120 tokens vs 3,000+ for raw file reads.
4. **BLAST (Pre-Mutation Safety Gate)**:
   - `mimori blast <coord> -d 3` | MCP: `mimori_blast(target=C, depth=3)`
   - Computes transitive ripple impact across callers, entry points, and test suites before editing.
5. **PROOF GATE (Validation)**:
   - `mimori memory lint ∧ mimori debt check == exit 0`
   - Enforces 30-line debt ceiling and validates 3-tuple debt schema.
6. **DEBT RECONCILIATION**:
   - `mimori debt sync`
   - Syncs in-code `# ponytail:` markers into `.agents/memory.md`, preserving `- accepted ...` waivers.

---

## SUBCOMMAND SPECIFICATIONS

### `map` — Architectural Centrality Map
```shell
mimori map [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
```
- In-degree PageRank centrality via power iteration ($d=0.85$, cap 100 iterations).
- `--focus <target>`: Personalized PageRank (PPR) biased toward target component.
- `--seed <term>`: Bias ranking toward symbols matching term.
- `--limit <N>`: Truncate output to top N symbols to fit token budgets.

### `slice` — Token-Dense Context Slicing
```shell
mimori slice <coordinate> [-f] [-i] [-n] [--budget <N>] [--json]
```
- Extract symbol or line range without reading entire file.
- Bodies exceeding 250 lines are head/tail truncated to protect token budgets.
- `-f`, `--follow-local`: Inline private local callee bodies from same file.
- `-i`, `--with-imports`: Include top-of-file imports for immediate dependency context.
- `-n`, `--numbered`: Prefix lines with 1-based coordinates (`L{n}: ...`).
- `--budget <N>`: Bounds markdown output; core slice preserved, droppable context sheds: inlined locals $\to$ callees $\to$ callers $\to$ imports.

### `find` — Hybrid Symbol/File Search
```shell
mimori find <pattern> [-s] [-f] [--limit <N>] [--json]
```
- Orders matches by exact match and in-degree PageRank.
- `-s`: Symbols only; `-f`: Files only.
- Fallback: Scans indexed file content when zero symbols/files match (count equals `rg -c`).

### `up` / `down` / `uses` — Graph Traversal
```shell
mimori up <target> [--json]    # Upstream callers & constructors (folds new X())
mimori down <target> [--json]  # Downstream callees invoked by target
mimori uses <target> [--json]  # Mentioners (property reads, type refs, args, templates)
```
- `up` folds constructor callers into class symbols; reports zero callers hint if unconstructed.
- `uses` tracks coarse dataflow without cluttering call graph centrality.

### `missing` — Absence Query
```shell
mimori missing <pattern> [--scope <dir>] [--defines <marker>] [--json]
```
- Walker sweep surfacing files under scope whose content lacks pattern (e.g. routes missing auth wrappers).

### `blast` — Transitive Ripple Analysis
```shell
mimori blast <target> [-d <N>] [--down] [--with-sinks <sinks>] [--json]
```
- Upstream reachability closure up to depth N.
- `--down`: Downstream dependency cone (delete-safety check).
- `--with-sinks <a,b,c>`: Appends parallel literal hits for unindexed sinks (e.g. `console.,logPath`).

### `doctor` — Repository Health
```shell
mimori doctor [--limit <N>] [--json]
```
- Health audit: file/symbol/edge counts, edge-resolution accounting, top hubs by fan-in, and dead-weight candidates (`likely dead` vs `needs human review`).

### `memory` — Living Project Memory Ledger
```shell
mimori memory [show|lint|resolve] [--section <sec>] [--budget <N>] [--json]
```
- Controls `.agents/memory.md`.
- `show`: Emits memory sections (`epics`, `debt`, `vocab`, `gotchas`).
- `lint`: Enforces 30-line ceiling, 3-tuple format, rejects completed checkboxes/strikethroughs.
- `resolve <pattern>`: Surgically deletes matched debt lines without altering adjacent formatting.

### `debt` — Ponytail Technical Debt Engine
```shell
mimori debt [list|check|sync] [--scope <dir>] [--json]
```
- Multi-threaded Rayon scanner discovering `# ponytail:` markers across all source languages.
- `list`: Surfaces all in-code markers, ceilings, and triggers.
- `check`: CI validation gate verifying non-empty ceilings, valid triggers, and 30-line ceiling.
- `sync`: Reconciles in-code markers into `.agents/memory.md` under `## KNOWN DEBT`, preserving operator waivers (`- accepted ...`) while purging stale markers.

### `dump` — Turn-0 Context Snapshot
```shell
mimori dump [--budget <N>] [--focus <target>] [--json]
```
- Packs PageRank architectural entry points, active technical debt, domain vocabulary, and empirical gotchas within specified token budget (default: 1500 tokens).

### `init` & `clean` — Storage Management
```shell
mimori init [--force]   # Initialize .mimori/ cache & .agents/ memory substrate
mimori clean [--all]    # Purge .mimori/index.db cache (Safe: .agents/ never touched)
```

---

## COORDINATE SYNTAX & RESOLUTION

```text
COORDINATES:
  path/to/file.rs:symbol           # Specific symbol in file
  path/to/file.ts:Class::method    # Member method in class
  path/to/file.rs:#L20-45          # Line range (zero-index disk read)
  symbol                           # Bare name (global workspace resolution)

RESOLUTION_ORDER:
  1. Exact workspace-relative path
  2. Path suffix on component boundary (e.g. alpha/mod.rs matches src/alpha/mod.rs)
  3. Basename alone

AMBIGUITY_INVARIANT:
  Candidates > 1 -> Exit 1 + Print candidate coordinates ranked by PageRank.
  Never guess or pick silently.
```

---

## MCP SERVER & TOOL DUALITY

Run stdio daemon: `mimori mcp [--workspace <dir>]`

| MCP Tool | CLI Equivalent | Key Arguments |
| :--- | :--- | :--- |
| `mimori_slice` | `mimori slice` | `coordinate`, `follow_local`, `with_imports`, `budget` |
| `mimori_map` | `mimori map` | `scope`, `focus`, `seed`, `limit` |
| `mimori_find` | `mimori find` | `pattern`, `symbols_only`, `files_only`, `limit` |
| `mimori_blast` | `mimori blast` | `target`, `depth`, `down`, `with_sinks` |
| `mimori_graph` | `mimori up/down/uses` | `target`, `direction` (`"up"`, `"down"`, `"uses"`) |
| `mimori_memory`| `mimori memory` | `action` (`"show"`, `"lint"`, `"resolve"`), `section`, `target`, `budget` |
| `mimori_debt` | `mimori debt` | `action` (`"list"`, `"check"`, `"sync"`), `scope` |

---

## PONYTAIL DEBT SYNTAX CONTRACT

In-code comments format:
```text
# ponytail: <what> <- <ceiling> -> <upgrade_trigger>
// ponytail: <what> <- <ceiling> -> <upgrade_trigger>
/* ponytail: <what> <- <ceiling> -> <upgrade_trigger> */
-- ponytail: <what> <- <ceiling> -> <upgrade_trigger>
```

- `<what>`: Concrete simplification or pragmatic deferral.
- `<ceiling>`: Verifiable operational threshold (e.g. `max 100 rps`, `depth <= 3`, `rows < 1000`).
- `<upgrade_trigger>`: Machine or human verifiable revisit trigger (e.g. `add redis pool`, `implement bulk API`).

---

## EXIT CODES

- `0`: Success / Verification Passed.
- `1`: Symbol not found, ambiguous coordinate, schema lint failure, debt ceiling breach, or database error.
- `2`: Invalid CLI arguments.
