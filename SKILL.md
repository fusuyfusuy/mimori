---
name: mimori
description: Zero-daemon project memory, structural symbol mapping, and activity tracking CLI (`mimori`). Use at session start for instant orientation and at completion for logging decisions and tasks.
---

# MIMORI(1) — Universal Agent Manual

## NAME
**mimori** — Zero-daemon agent context snapshotting, AST symbol mapping, context slicing, and activity tracking CLI stored in `.mimori/`. Standard library only.

## SYNOPSIS
```text
mimori [--test] <subcommand> [options] [arguments]

mimori dump    [--file [PATH]] [-S STR] [--no-tests] [--kind TIER]
               [--focus TOKENS] [-s DIRS] [--budget BUDGET]
mimori map     [--stdout] [-S STR] [--no-tests] [--kind TIER]
               [--focus TOKENS] [-s DIRS] [--format md|json] [--budget BUDGET]
mimori slice   <target> [-l N] [-o L] [-f] [-s DIRS]
mimori todo    [list|add|start|done|reopen|rm] [target] [-p PRIO] [-t TAG]
               [--status STAT] [--start] [--idea] [--plain]
mimori idea    [list|add|promote|rm] [target] [-t TAG] [--plain]
mimori debt    [list|sync|check] [-s DIRS] [--plain]
mimori log     --action ACT --summary TXT [--files F1,F2]
mimori history [--limit N]
mimori clean   [--all]
mimori init
```

## QUICK REFERENCE & DISPATCH TABLE

| Action | Command | When to Use |
| :--- | :--- | :--- |
| **Warmup Snapshot** | `[ -d .mimori ] \|\| mimori init && mimori dump --file` | Session start (Turn 0), `/mimori` on new repo, subagent kickoff. |
| **Direct Dump Output** | `mimori dump` | Stream context directly to stdout/pipes without writing to disk. |
| **Print Structural Map** | `mimori map --stdout` | Inspect ranked symbols, callers, and import graph in terminal. |
| **Grep-Seeded Map** | `mimori map --stdout --seed "<str>"` | Topic-Sensitive PageRank elevating polymorphic subtypes & literal tokens. |
| **Noise-Filtered Map** | `mimori map --stdout --no-tests --kind backend` | Strip test suites (`__test__`) and UI trees (`components/`) from graph. |
| **Context Slice** | `mimori slice <file>[:<sym>\|#L<s-e>]` | Deterministic context slice with 1-hop lineage; adaptive 250-line ceiling. |
| **Inline Helper Callees** | `mimori slice <file>:<sym> -f` | Slice target symbol and automatically inline internal helper functions. |
| **Save Structural Map** | `mimori map` | Update `.mimori/repo_map.md` on disk for external agent tools. |
| **Initialize Workspace** | `mimori init` | First-time project setup; scaffolds `.mimori/` directory and templates. |
| **List Tasks** | `mimori todo` | Review active tasks, in-progress items, and backlog. |
| **Add Task** | `mimori todo add "<task>"` | Track new discrete unit of work. |
| **Update Task State** | `mimori todo start <id>` / `done <id>` | Mark item in-progress `[/]`, complete `[x]`, reopen `[ ]`, or rm. |
| **Track Future Ideas** | `mimori idea add "<idea>"` | Store backlog ideas `[?]` without cluttering active todo. |
| **Promote Idea** | `mimori idea promote <id>` | Move backlog idea into active tasks `[ ]`. |
| **Audit Ponytail Debt** | `mimori debt [--scope <dir>]` | List all in-code `# ponytail:` / `// ponytail:` markers. |
| **Sync Debt Ledger** | `mimori debt sync` | Reconcile in-code markers into `.mimori/memory.md` (`## KNOWN DEBT`). |
| **Validate Debt CI** | `mimori debt check` | Exit 0 if all markers have valid triggers, exit 1 if malformed. |
| **Log Repo Action** | `mimori log --action <a> --summary <s>` | Log discrete action, tool execution, or edit to `activity.jsonl`. |
| **View History** | `mimori history [--limit <N>]` | Audit recent cross-session activity log entries. |
| **Prune Cache** | `mimori clean` / `mimori clean --all` | Purge `.mimori/.cache/` (context snapshot and SQLite `ast.db`). |

---

## DESCRIPTION

`mimori` serves as the zero-daemon memory, structural symbol mapper, and execution journal for coding agents. It enforces the Ponytail lazy senior developer ruleset (standard library only, cyclomatic complexity $\le 10$, depth $\le 3$, zero background daemons).

Key architectural subsystems include:
1. **Zero-Daemon SQLite AST Delta Cache**: Stores symbol graphs and file metadata in `.mimori/.cache/ast.db` (WAL mode, busy timeout 5s). Only modified files are re-parsed based on `(mtime_ns, size)`.
2. **Topic-Sensitive PageRank**: Combines fast literal search (`rg -l -i -F`) with AST dependency graphs. Direct token matches receive personalized teleportation vectors ($v[i] = 1/|S|$) and additive score bonuses ($+8.0$).
3. **Pre-Graph Noise Filtering**: Drops test suites (`__test__`, `tests/`) and segregates architectural tiers (`--kind backend|frontend`) before graph ranking to prevent noise from inflating PageRank.
4. **Adaptive Context Slicing**: Renders full symbol implementations by default, adaptively emits up to 250 lines for whole files, parses GitHub-style coordinate ranges (`#L100-L180`), and inlines local private callees (`--follow-local`).

---

## SUBCOMMANDS & OPTIONS

### `dump` — Session Context Snapshot
Generates unified, budget-managed context containing working git state, project memory, decisions, tasks, ranked symbol map, and recent activity.

```text
mimori dump [options]
```

**Options**:
- `--file [PATH]`, `-o [PATH]`, `--out [PATH]`  
  Write context snapshot to in-scope file (`.mimori/.cache/context.md`) or optional custom path.
- `--seed <token>`, `-S <token>`  
  Seed Topic-Sensitive PageRank with search token to elevate polymorphic handlers (Env: `MIMORI_SEED`).
- `--no-tests`  
  Exclude test files and test suites from map (Env: `MIMORI_NO_TESTS=1`).
- `--kind {backend,frontend,all}`  
  Filter candidate files by architectural layer (Env: `MIMORI_KIND`).
- `--focus <tokens>`  
  Comma-separated list of paths or keywords to expand to full detail along with 1-hop graph neighbors (Env: `MIMORI_FOCUS`).
- `--scope <dirs>`, `-s <dirs>`  
  Repository-relative subtree or comma-separated subtrees to isolate analysis. Internal absolute paths are automatically relativized (Env: `MIMORI_SCOPE`).
- `--budget <preset|int>`  
  Character cap for snapshot: `default` (24,000), `large` (60,000), `immense` (200,000), `unlimited`, or integer (Env: `MIMORI_BUDGET`).

**Budget Degradation Hierarchy**:
1. *Memory* (`memory.md`): Invariants & gotchas kept first; epics truncated last.
2. *Decisions* (`decisions.md`): All titles kept; newest expanded; superseded unexpanded.
3. *Tasks* (`tasks.md`): In-progress `[/]` and active `[ ]` prioritized; completed `[x]` collapsed to count.
4. *Map* (`repo_map.md`): Low-ranked files collapsed into directory counts.
5. *Activity* (`activity.jsonl`): Summaries clipped to 160 chars; oldest dropped with notice.

---

### `map` — Structural AST & Import Graph
Parses top-level symbols (classes, functions, methods, line numbers) and computes PageRank from the import graph.

```text
mimori map [options]
```

**Options**:
- `--stdout`  
  Print generated markdown map to standard output instead of updating `.mimori/repo_map.md`.
- `--seed <token>`, `-S <token>`  
  Search token to seed topic-sensitive PageRank (Env: `MIMORI_SEED`).
- `--no-tests`  
  Exclude test suites (`tests/`, `__tests__/`, `*.test.*`, `*_test.*`) from graph (Env: `MIMORI_NO_TESTS=1`).
- `--kind {backend,frontend,all}`  
  Architecture kind filter: `backend` excludes UI trees (`components/`, `views/`, `.tsx`, `.jsx`, `.svelte`, `.vue`); `frontend` restricts to UI components (Env: `MIMORI_KIND`).
- `--focus <tokens>`  
  Comma-separated substrings to pull into full detail; collapses remaining files to directory summaries.
- `--scope <dirs>`, `-s <dirs>`  
  Subtree or comma-separated subtrees to scan (Env: `MIMORI_SCOPE`).
- `--format {md,json}`  
  Output format: `md` (markdown, default) or `json` (includes `score`, `pagerank`, `in_degree`, `seed_match`).
- `--budget <preset|int>`  
  Character cap for map rendering (default: 8,000 chars).

---

### `slice` — Deterministic 1-Hop Context Slicing
Extracts targeted coordinates, 1-hop lineage (callers + dependencies), contract, and exact source code without reading entire files into context.

```text
mimori slice <target> [options]
```

**Target Syntax**:
- `file.py:symbol_name` — Function, class, or method inside file.
- `file.ts#L100-L180` or `file.ts:100-180` — Exact line coordinate range.
- `file.ts#L125` or `file.ts:125` — Peek window starting at line 125.
- `file.py` — Whole file (adaptive ceiling: files $\le 250$ lines rendered completely).

**Options**:
- `--lines <N>`, `-l <N>`  
  Maximum source lines to render (default: full symbol, 250 lines for whole file, or exact coordinate range).
- `--offset <L>`, `-o <L>`  
  1-based starting line offset for manual window pagination.
- `--follow-local`, `-f`  
  Scan symbol body and inline up to 4 internal private/helper functions defined within the same file under `## Local Callees (inlined)`.
- `--scope <dirs>`, `-s <dirs>`  
  Scope file resolution to subtree.

---

### `todo` & `idea` — Zero-Daemon Task Tracking
Manages active tasks, in-progress items, and deferred ideas in `.mimori/tasks.md`. `mimori task` is an alias for `mimori todo`.

```text
mimori todo [action] [target] [options]
mimori idea [action] [target] [options]
```

**Actions (`todo`)**:
- `list` (default): Display active and in-progress tasks.
- `add "<description>"`: Create a new task.
- `start <id|pattern>`: Move item to In Progress `[/]`.
- `done <id|pattern>`: Mark item Completed `[x]` with current ISO date.
- `reopen <id|pattern>`: Move item back to Active `[ ]`.
- `rm <id|pattern>`: Permanently remove item.

**Actions (`idea`)**:
- `list` (default): Display future ideas and backlog proposals `[?]`.
- `add "<description>"`: Append a new idea to backlog.
- `promote <id|pattern>`: Promote backlog idea directly into active tasks `[ ]`.
- `rm <id|pattern>`: Remove idea.

**Options**:
- `--prio {high,med,low,urgent}`, `-p`  
  Task priority level.
- `--tag <tag>`, `-t <tag>`  
  Tag(s) to attach to task (repeatable).
- `--status {todo,in_progress,done,idea,all}`, `-s`  
  Filter tasks by status.
- `--start`  
  When adding, insert directly into In Progress `[/]`.
- `--idea`  
  When adding via `todo`, insert into Future Ideas `[?]`.
- `--plain`  
  Emit plain text without ANSI terminal formatting.

---

### `debt` — Ponytail Technical Debt Ledger
Scans source files for `# ponytail:` and `// ponytail:` deliberate shortcuts formatted as:
`# ponytail: <what> <- <ceiling> -> <upgrade trigger>`

```text
mimori debt [action] [options]
```

**Actions**:
- `list` (default): Display all debt items with file locations, ceilings, and triggers.
- `sync`: Reconcile code markers into `.mimori/memory.md` (`## KNOWN DEBT`). Auto-prunes resolved markers; preserves manual waivers starting with `accepted ...`.
- `check`: Automated CI gate. Validates every marker has a valid `-> <trigger>`. Exits 0 if compliant, 1 if malformed.

**Options**:
- `--scope <dirs>`, `-s <dirs>`  
  Scope technical debt scan to specific directory or packages.
- `--plain`  
  Disable ANSI color formatting.

---

### `log` & `history` — Repository Activity Journal
Records and inspects high-signal actions and tool executions in `.mimori/activity.jsonl`.

```text
mimori log --action <name> --summary <text> [--files <f1,f2>]
mimori history [--limit <N>]
```

**Options (`log`)**:
- `--action <name>`  
  Short categorical identifier (e.g. `refactor-auth`, `benchmark-sweep`, `debt-sync`).
- `--summary <text>`  
  High-level 1-line caveman summary (<160 chars) explaining what changed and why.
- `--files <paths>`  
  Comma-separated list of modified or inspected files.

**Options (`history`)**:
- `--limit <N>`  
  Number of recent activity entries to display (default: 10).

---

### `clean` & `init` — Lifecycle Maintenance
```text
mimori clean [--all]
mimori init
```

- `clean`: Removes in-scope `.mimori/.cache/context.md` and legacy snapshots.
- `clean --all`: Completely wipes all cached snapshots and resets `.mimori/.cache/ast.db`.
- `init`: Initializes `.mimori/` scaffolding (`memory.md`, `decisions.md`, `tasks.md`, `activity.jsonl`, `.gitignore`) and runs first map generation.

---

## ENVIRONMENT

| Variable | Description |
| :--- | :--- |
| `MIMORI_SEED` | Default search token for topic-sensitive PageRank. |
| `MIMORI_NO_TESTS` | If `1`, `true`, `yes`, excludes test suites from graph. |
| `MIMORI_KIND` | Architectural layer filter (`backend`, `frontend`, `all`). |
| `MIMORI_FOCUS` | Comma-separated paths/keywords to focus by default. |
| `MIMORI_SCOPE` | Subtree or comma-separated subtrees to scope repo scan. |
| `MIMORI_BUDGET` | Character budget preset or integer for map and dump. |
| `AGENT_NAME` | Author name for `activity.jsonl` records (default: `agent`). |
| `MIMORI_CACHE_DIR` | Custom directory override for context snapshot file. |

---

## FILES & WORKSPACE LAYOUT

```text
<repo-root>/
├── .mimori/
│   ├── memory.md        # Domain rules, invariants, gotchas, ## Flagged ambiguities, ## KNOWN DEBT
│   ├── decisions.md     # Architecture Decision Records (Context, Decision, Consequences)
│   ├── tasks.md         # Priority-budgeted task list ([ ], [/], [x]) and ideas ([?])
│   ├── repo_map.md      # AST structural map output from `mimori map`
│   ├── activity.jsonl   # Append-only immutable repository action journal
│   ├── .gitignore       # Auto-shields .cache/, .locks/, and *.tmp* from git tracking
│   ├── .locks/          # Multi-process advisory file locks (*.lock)
│   └── .cache/          # In-scope execution cache
│       ├── context.md   # Unified warmup snapshot from `dump --file`
│       └── ast.db       # Zero-daemon SQLite AST delta cache (WAL mode)
```

---

## EXIT STATUS

| Exit Code | Meaning |
| :--- | :--- |
| `0` | Successful execution, valid check, or clean self-test. |
| `1` | General error (target not found, invalid subcommand, or `debt check` failed). |
| `2` | Parameter boundary violation (e.g. `--lines` or `--offset` less than 1). |
| `SystemExit` | Scope path escapes repository root traversal check. |
