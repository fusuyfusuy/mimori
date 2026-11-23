---
name: mimori
description: >
  Zero-config AST code-intelligence, symbol-graph, context-slicing, and JIT in-repo agent memory engine.
  Use for symbol search, 1-hop AST slicing, module-stratified PageRank mapping, blast-radius analysis,
  Turn-0 context dumping, and scope-anchored project memory/mimodept debt tracking. Native CLI and MCP stdio.
---

# MIMORI(1) — AST Intelligence & In-Repo Memory Substrate

```text
KERNEL:
  TARGET: Fast In-Repo Orientation, Blast-Radius Analysis & JIT Agent Memory
  BINARY: ~/.local/bin/mimori (CLI) | mimori mcp (JSON-RPC 2.0 stdio)
  VERSION: 3.2.0
  INVARIANTS:
    1_HASH:       Content-Hash Governed (FNV-1a) — warm MCP uses POSIX ino+size+mtime+ctime stat gate
    2_PURITY:     Deterministic & Non-Interactive — byte-identical JSON & Markdown, zero background daemons
    3_CONFINED:   Workspace Confinement strictly enforced (CLI anchored to cwd/--workspace; lexical .. check)
    4_STORAGE:    .mimori/ (ephemeral atomic SQLite cache) | .agents/memory.md (git-tracked memory) | docs/adr/ (indexed)
```

## SYNOPSIS

Global flags: `--json`, `-w | --workspace <dir>`.

```shell
mimori init    [--force]
mimori map     [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice   <coordinate>... [-f|--follow-local] [-i|--with-imports] [-n|--numbered] [--budget <N>] [--json]
mimori find    <pattern> [-s|--symbols-only] [-f|--files-only] [--scope <dir>] [--limit <N>] [--json]
mimori up      <target> [--json]
mimori down    <target> [--json]
mimori uses    <target> [--json]
mimori missing <pattern> [--scope <dir>] [--defines <lit>] [--json]
mimori blast   <target>... [-d|--depth <N>] [--down] [--with-sinks <a,b,c>] [--json]
mimori diff    [--staged] [--base <rev>] [-d|--depth <N>] [--json]
mimori doctor  [--limit <N>] [--json]
mimori memory  [show|add|lint|check|resolve] [--section <sec>] [--scope <glob>] [--sym <name>] [--all] [--budget <N>] [--json]
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
   - `mimori dump --budget 1500` | MCP: `mimori_dump(budget=1500)`
   - Injects `docs/adr/` titles, domain vocabulary, gotchas, active debt, and module-stratified architectural map within reserved budget floors (>=40% map).
2. **CANOPY (Topography)**:
   - `mimori map` (defaults to top 30 stratified public-API/entry-point symbols; `--limit 0` for all) | MCP: `mimori_map(limit=30)`
   - Covers every core subsystem without reading raw files.
3. **SLICE (Targeted Reading + JIT Memory)**:
   - `mimori slice <coord> -f -i -n` | MCP: `mimori_slice(coordinate=C, follow_local=true, numbered=true)`
   - Emits exact declaration body, 1-hop callers/callees, inlined private helpers, and any `@scope:`/`@sym:` memory notes matching the file or symbol (~150–400 tokens vs 3,000+ for raw file reads).
4. **BLAST (Pre-Mutation Safety Gate)**:
   - `mimori blast <coord> -d 3` | MCP: `mimori_blast(target=C, depth=3)`
   - Computes transitive ripple impact across callers, entry points, test suites, scoped memory, and emits explicit `unverified_member_calls` warnings whenever unresolved cross-file `.method()` call sites remain.
5. **PROOF GATE (Validation)**:
   - `mimori memory lint && mimori debt check` (both exit `0` when schema/syntax valid; `>30` debt items emit non-blocking `WARN`).
6. **MEMORY & DEBT RECONCILIATION**:
   - `mimori memory add --section gotchas --scope "src/mod/*" "<invariant>"`
   - `mimori debt sync` (syncs Tree-sitter comment `# mimodept:` markers into `.agents/memory.md`, preserving all `- accepted ...` waivers without truncation).

---

## SUBCOMMAND SPECIFICATIONS

### `map` — Module-Stratified Centrality Map
```shell
mimori map [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
```
- In-degree PageRank centrality ($d=0.85$, cap 100 iterations) with test quarantine via AST `IS_TEST` flags.
- Default `--limit` is `30` (`--limit 0` returns all symbols). Unfocused maps exclude test symbols (unless `--scope` targets tests), elevate entry points/public API over private <=5 LOC zero-out-degree helpers, and stratify round-robin across top-level module buckets.
- `--focus <target>`: Strict unique coordinate resolution for Personalized PageRank (PPR).
- `--seed <term>`: Bias ranking toward symbols whose name or path matches `term`.

### `slice` — Token-Dense Context Slicing + JIT Scoped Memory
```shell
mimori slice <coordinate>... [-f] [-i] [-n] [--budget <N>] [--json]
```
- Extract one or more symbols or 1-based line ranges in a single call (MCP `coordinate` accepts comma-separated string or `string[]`).
- Automatically injects `@scope:`/`@sym:` notes from `.agents/memory.md` matching the target file or symbol.
- Bodies exceeding 250 lines are head/tail truncated; `--follow-local` stores typed `local_callees` (max 16, each truncated cleanly without in-band string sentinels).
- `--budget <N>`: Strictly fits rendered markdown (including truncation notice) within `N` tokens whenever core fits; droppable priority: `inlined local callees -> callees -> callers -> imports -> memory notes`.

### `find` — Hybrid Symbol/File/Literal Search
```shell
mimori find <pattern> [-s] [-f] [--scope <dir>] [--limit <N>] [--json]
```
- Supports `|` or `,` multi-pattern batching; validates `--scope` strictly against workspace confinement.
- Orders exact matches and declarations above literal fallbacks; per-term fallback scans indexed files (capped at 500 hits / 8 MiB per file) and resolves each hit to its innermost enclosing AST symbol.

### `up` / `down` / `uses` — Strict Graph Traversal
```shell
mimori up <target> [--json]    # Upstream callers & constructors (folds new X() / X::new / X::__init__)
mimori down <target> [--json]  # Downstream callees invoked by target
mimori uses <target> [--json]  # Non-call mentioners (property reads, type refs, args, templates)
```
- Fails closed (exit `1`) on missing or ambiguous coordinates.
- MCP `mimori_graph` accepts `string | string[]` batch `target`s.

### `missing` — Absence Query
```shell
mimori missing <pattern> [--scope <dir>] [--defines <marker>] [--json]
```
- Walker sweep surfacing supported source files under confined `scope` whose content lacks all `|` alternatives (handles non-UTF8 via lossy decoding; rejects escaping scopes or empty patterns/defines).

### `blast` — Transitive Ripple Analysis
```shell
mimori blast <target>... [-d <N>] [--down] [--with-sinks <sinks>] [--json]
```
- Strict target resolution; upstream reachability closure up to depth N with `d=1` call-site lines, literal test mentions, scoped memory notes, and `unverified_member_calls` warning whenever querying a method with unresolved untyped cross-file `.method()` call sites.
- `--down`: Downstream dependency cone (delete-safety check).
- `--with-sinks <a,b,c>`: Literal sweep scoped to the blast cone files (capped at 100 hits).

### `diff` — Git Diff Blast & Test Impact
```shell
mimori diff [--staged] [--base <rev>] [-d <N>] [--json]
```
- Runs `git diff -U0` with normalized `a/`/`b/` prefixes (`--no-textconv`, 30s timeout) and a strict unified-diff state machine (immune to body lines starting with `+++ `, tracks deleted & renamed files, unescapes C-quoted paths).
- Intersects hunks with indexed symbols and reports mutated symbols, deleted files, combined upstream blast cone, scoped memory notes, affected test suites, and debt/memory gate status.

### `doctor` — Repository Health
```shell
mimori doctor [--limit <N>] [--json]
```
- File/symbol/edge counts, edge-resolution accounting, top production hubs by fan-in (excluding test symbols), and dead-weight candidates excluding tests (`IS_TEST`), trait impls (`IS_TRAIT_IMPL`), serde/attribute hooks (`IS_ATTR_ENTRY`), framework entries, and methods with unresolved cross-file member calls.

### `memory` — Living Project Memory Ledger
```shell
mimori memory [show|add|lint|check|resolve] [--section <sec>] [--scope <glob>] [--sym <name>] [--all] [--budget <N>] [--json]
```
- Controls `.agents/memory.md` with advisory write locking and `O_EXCL` + `fsync` atomic renames.
- `show`: Emits full ledger or specific section (`epics`, `debt`, `vocab`, `gotchas`).
- `add <text> [--section <sec>] [--scope <glob>] [--sym <name>]`: Appends validated single-line entry (with optional `[@scope: ...]` / `[@sym: ...]` JIT anchor).
- `lint` / `check`: Enforces 3-tuple debt schema and forbids strikethrough/checkboxes (`exit 1` on syntax errors; soft `MEM_LINT_WARN` + `exit 0` above 30 items).
- `resolve <pattern> [--all]`: Surgically deletes matched debt item(s); fails closed on 0 matches, overly broad punctuation-only patterns, or >1 match without `--all`.

### `debt` — Tree-Sitter Mimodept Technical Debt Engine
```shell
mimori debt [list|check|sync] [--scope <dir>] [--json]
```
- Extracts `mimodept:` (and typo `mimodebt:`) strictly from Tree-sitter comment AST nodes for Rust/TS/JS/Python/Go (plus allowlisted line-comment config/SQL/shell files); ignores strings, raw strings, docstrings, markdown, and dropped legacy `ponytail:` markers.
- `list`: Surfaces in-code markers, ceilings, triggers, and unreadable-file warnings.
- `check`: Fails closed (`exit 1`) on invalid marker syntax, unreadable files, or malformed ledger confinement; emits non-blocking `DEBT_CHECK_WARN` (`exit 0`) above 30 items.
- `sync`: Reconciles in-code markers into `.agents/memory.md` under `## KNOWN DEBT`, preserving ALL `- accepted ...` waivers and markers without silent truncation; refuses if duplicate `## DEBT` headings exist.

### `dump` — Turn-0 Context Snapshot
```shell
mimori dump [--budget <N>] [--focus <target>] [--json]
```
- Packs module-stratified architectural map (>=40% reserved budget floor), `docs/adr/*.md` titles, epics, domain vocabulary, empirical gotchas, and active debt strictly within `--budget` (default 1500 tokens).

### `init` & `clean` — Storage Management
```shell
mimori init [--force]   # Atomically validate paths, scaffold .agents/memory.md, prepare .mimori/ & .gitignore
mimori clean [--all]    # Purge .mimori/ cache files (refuses symlinks & disallowed roots; .agents/ never touched)
```

---

## COORDINATE SYNTAX & RESOLUTION

```text
COORDINATES:
  path/to/file.rs:symbol           # Specific symbol in file (also accepts path/to/file.rs#symbol)
  path/to/file.ts:Class::method    # Member method in class
  path/to/file.rs:#L20-45          # 1-based line range (also accepts #L20-L45 or :L20-45; L0 rejected)
  symbol                           # Bare name (global workspace resolution)

RESOLUTION_ORDER:
  1. Exact workspace-relative path
  2. Path suffix on strict component boundary (e.g. alpha/mod.rs matches src/alpha/mod.rs)

AMBIGUITY_INVARIANT:
  Candidates > 1 -> Exit 1 + Print candidate coordinates ranked by PageRank.
  Never guess or pick silently.
```

---

## MCP SERVER, CLI DUALITY & `--json` OUTPUT CONTRACTS (`.d.ts`)

Run stdio server: `mimori mcp [--workspace <dir>]`
Note: MCP tools (`mimori_*`) return formatted Markdown text; CLI `--json` returns the exact JSON schemas below:

```ts
declare namespace mimori {
  type SymbolKind =
    | "function" | "method" | "struct" | "class" | "interface" | "trait"
    | "enum" | "type" | "variable" | "module" | "constant" | "field";

  interface SymbolEntry {
    name: string; kind: string; file?: string; coordinate: string;
    start_line: number; end_line: number; signature: string; centrality: number;
  }
  interface InlinedCallee {
    name: string; start_line: number; end_line: number; content: string;
  }
  interface BlastNode {
    name: string; kind: string; file: string; coordinate: string; depth: number;
    call_lines?: number[]; is_entry_point: boolean; is_sink: boolean; is_test: boolean; is_value_use: boolean;
  }
  interface MapResult {
    scope: string | null; focus: string | null;
    modules: Array<{ file: string; symbols: SymbolEntry[] }>;
    total_symbols: number; truncated_from?: number; resolved: number;
    ambiguous_dropped: number; unresolved: number; external: number;
    pagerank_iters: number; pagerank_converged: boolean;
    indexed_files?: number; crawled_files?: number; unindexed_exts?: string[];
  }

  // CLI: mimori dump [--budget <N>] [--focus <target>] --json | MCP: mimori_dump({ budget?, focus?, workspace_dir? }) -> string
  type DumpJson = {
    budget: number; estimated_tokens: number; epics?: string; vocab_gotchas: string | null;
    active_debt: string[]; debt_count: number; adrs?: string[]; map: MapResult | null; markdown: string;
  };
  // CLI: mimori slice <coord>... [-f] [-i] [-n] [--budget <N>] --json | MCP: mimori_slice({ coordinate: string | string[], follow_local?, with_imports?, numbered?, budget?, workspace_dir? }) -> string
  type SliceJson = {
    coordinate: string; file: string; line_range: [number, number] | null; content: string;
    callers: string[]; callees: string[]; total_lines: number;
    imports?: string[]; local_callees?: InlinedCallee[]; memory_notes?: string[];
    symbol: null | {
      name: string; kind: SymbolKind; file: string; start_line: number; end_line: number;
      signature: string; body?: string; centrality: number;
      calls: string[]; mentions: string[]; call_counts: Record<string, number>;
      member_calls: string[]; external_imports: string[]; flags?: number;
    };
  };
  // CLI: mimori find <pattern> [-s] [-f] [--scope <dir>] [--limit <N>] --json | MCP: mimori_find({ pattern, scope?, symbols_only?, files_only?, limit?, workspace_dir? }) -> string
  type FindJson = { query: string; matches: SymbolEntry[]; truncated_from?: number };
  // CLI: mimori map [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] --json | MCP: mimori_map({ scope?, focus?, seed?, limit?, workspace_dir? }) -> string
  type MapJson = MapResult;
  // CLI: mimori up|down|uses <target> --json | MCP: mimori_graph({ target: string | string[], direction: "up" | "down" | "uses", workspace_dir? }) -> string
  type GraphJson = { target: string; callers?: string[]; callees?: string[]; mentioners?: string[]; value_uses?: string[] };
  // CLI: mimori blast <target>... [-d <N>] [--down] [--with-sinks <a,b>] --json | MCP: mimori_blast({ target: string | string[], depth?, down?, with_sinks?, workspace_dir? }) -> string
  type BlastJson = {
    target: string; direction: string; depth_limit: number;
    affected: BlastNode[]; entry_points: string[]; test_suites: string[];
    value_uses: BlastNode[]; literal_test_mentions?: string[];
    unverified_member_calls?: string; memory_notes?: string[];
    sinks?: string[]; sink_hits?: string[]; sink_hits_truncated?: boolean;
  };
  // CLI: mimori diff [--staged] [--base <rev>] [-d <N>] --json | MCP: mimori_diff({ staged?, base?, depth?, workspace_dir? }) -> string
  type DiffJson = {
    touched_files: string[]; deleted_files?: string[];
    mutated_symbols: Array<{ name: string; kind: string; file: string; coordinate: string; start_line: number; end_line: number }>;
    affected: BlastNode[]; entry_points: string[]; test_suites: string[];
    literal_test_mentions: string[]; memory_notes?: string[];
    debt_status: string; memory_status: string;
  };
  // CLI: mimori missing <pattern> [--scope <dir>] [--defines <lit>] --json | MCP: mimori_missing({ pattern, scope?, defines?, workspace_dir? }) -> string
  type MissingJson = { pattern: string; scope: string | null; defines: string | null; missing: string[] };
  // CLI: mimori memory [show|add|lint|resolve] [--section <sec>] --json | MCP: mimori_memory({ action: "show" | "add" | "lint" | "resolve", section?, text?, scope?, sym?, target?, all?, budget?, workspace_dir? })
  type MemoryShowJson = {
    debt_count: number; epics: string; vocab: string; gotchas: string; vocab_gotchas: string;
    debt_items: Array<{ line_number: number; what: string; why: string; trigger: string; is_accepted: boolean; raw: string }>;
  };
  // CLI: mimori debt [list|check|sync] [--scope <dir>] --json | MCP: mimori_debt({ action: "list" | "check" | "sync", scope?, workspace_dir? })
  type DebtListJson = Array<{ file: string; line: number; what: string; ceiling: string; trigger: string; valid_trigger: boolean; error_reason: string | null; raw: string }>;
  // CLI: mimori doctor [--limit <N>] --json | MCP: mimori_doctor({ limit?, workspace_dir? }) -> string
  type DoctorJson = {
    files: number; symbols: number; edges: number; isolated: number;
    likely_dead: Array<{ name: string; kind: string; file: string; coordinate: string; centrality: number }>;
    needs_review: Array<{ name: string; kind: string; file: string; coordinate: string; centrality: number }>;
    likely_dead_total?: number; needs_review_total?: number;
    top_hubs: Array<{ name: string; kind: string; coordinate: string; fan_in: number; centrality: number }>;
    resolved: number; ambiguous_dropped: number; unresolved: number; external: number;
    pagerank_iters: number; pagerank_converged: boolean;
  };
}
```

---

## MIMODEPT DEBT SYNTAX CONTRACT

In-code comments format (extracted from real AST comment nodes only):
```text
# mimodept: <what> <- <ceiling> -> <upgrade_trigger>
// mimodept: <what> <- <ceiling> -> <upgrade_trigger>
/* mimodept: <what> <- <ceiling> -> <upgrade_trigger> */
-- mimodept: <what> <- <ceiling> -> <upgrade_trigger>
```

- `<what>`: Concrete simplification or pragmatic deferral.
- `<ceiling>`: Verifiable operational threshold (e.g. `max 100 rps`, `depth <= 3`, `rows < 1000`).
- `<upgrade_trigger>`: Machine or human verifiable revisit trigger (e.g. `add redis pool`, `implement bulk API`).

---

## EXIT CODES

- `0`: Success / Verification Passed (includes non-blocking soft debt ceiling warning `> 30`).
- `1`: Symbol not found, ambiguous coordinate, workspace escape, schema lint failure, debt syntax/read error, or database error.
- `2`: Invalid CLI arguments.
