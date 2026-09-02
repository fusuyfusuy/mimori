# mimori (三森) — Zero-Daemon Agent Memory & Live AST Map

[![CI](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml/badge.svg)](https://github.com/fusuyfusuy/mimori/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Python 3.10+](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Zero Dependencies](https://img.shields.io/badge/dependencies-0%20(stdlib%20only)-green.svg)](https://github.com/fusuyfusuy/mimori) [![Tiered AST](https://img.shields.io/badge/AST-tiered%20%28works%20without%2C%20better%20with%29-blue.svg)](https://github.com/fusuyfusuy/mimori#tiered-ast-engines--better-with-works-without)

> **Works with zero dependencies. Better with `tree-sitter` if you have it — never required.**

> **One file. Zero daemons. Zero deps. Instant orientation.**
> Single `mimori dump --file` replaces 20 blind `grep/find/read` calls — ranked symbols, ADRs, tasks & debt in <1.5s.

`mimori` is a single Python file that gives any coding agent (Claude Code, Antigravity, Pi, OpenCode, Aider) instant structural awareness in any repo — no LSP, no vector DB, no background process. Deterministic PageRank on your import graph, not hallucinated embeddings.

```bash
mimori dump --file                                    # 6 layers, 12-24KB, <0.2s → saves 50,000 tokens
mimori map --seed "stack deploy" --no-tests           # Topic-Sensitive PageRank; elevates polymorphic variants
mimori slice compose.ts:createCommand --follow-local  # 120 tokens vs 3,000 for whole file; inlines helpers
```

---

## Why mimori exists

| Without mimori | With mimori |
|---|---|
| `grep -r "auth"` → 80 hits, manual triage, no ranking | `mimori map --focus auth` → PageRank-ranked: `lib/utils.ts ←29`, `types/* ←27` in 0.3s |
| `read index.astro` → 1,802 lines / 14,000 tokens of SSR+CSS+SVG | `mimori slice facets.ts` → 73 lines / 120 tokens, callers+deps+contract |
| 15–25 min wall time, 30–40 tool calls, 100–150KB context | ~90s, 14 calls, ~30KB — **10–15× faster, 3–5× leaner** |
| Misses `hermes-credential.ts`, `NegotiationCounterpartyBinding` (no import) | Deterministic: line-anchored `adapter.ts:40`, `hermes-credential.ts:1` — never misses |

*Measured, not vibes. Real public repos, 1,500–6,515 files:*

| Repo | Scale | Time | Tokens | Tool calls | Source |
|---|---|---|---|---|---|
| **Dokploy** (`Dokploy/dokploy`) | 1,562 files, 211K LOC | **30 turns vs 47 turns** | **619K vs 1.17M (47% less)** | 29 vs 46 (0 vs 22 fallback reads) | [`artifacts/1.4.0/dokploy-swarm-test-updated.md`](artifacts/1.4.0/dokploy-swarm-test-updated.md) |
| **herdr** (`herdr/herdr`) | Rust terminal manager | **10.1m vs 14.5m** | **2.22M vs 5.23M (2.35× less)** | 29 vs 84 (0 subagents needed) | [`artifacts/mimori-vs-without-herdr.md`](artifacts/mimori-vs-without-herdr.md) |
| **opencode** (`anomalyco/opencode`) | 6,515 files, 680K LOC | not measured | **880 vs 10,900 (12×)** | 3 vs 6–10 | [`artifacts/mimori-vs-without-opencode.md`](artifacts/mimori-vs-without-opencode.md) |

Compounding cost: without mimori, 50K+ tokens stay in history and tax *every* future turn. With mimori, the baseline is ~4K.

---

## What you get

**One command, six layers:**

```
mimori dump --file   →  .mimori/.cache/context.md
```

1. **Git state** — branch, dirty files, last commit (never fabricates `main` on failure)
2. **Memory** — invariants, gotchas, domain rules from `.mimori/memory.md`
3. **Decisions** — ADRs (all titles always visible, superseded collapsed)
4. **Tasks** — `[/]` in-progress + `[ ]` active prioritized, `[x]` collapsed to count
5. **Map** — PageRank-ranked symbols with importers, churn, entry points, and `--seed` matches
6. **Activity** — last 5 `mimori log` entries, budget-capped (1,200 chars)

All budget-aware: `memory → decisions → tasks → map → activity`. Nothing silently dropped — every truncation prints `N of M shown`.

**Surgical extraction:**

```bash
# 1-hop lineage: callers, deps, signature, bounded code
mimori slice src/auth/token.py:verify_jwt             # exact symbol (full body rendered by default)
mimori slice compose.ts:createCommand --follow-local  # inlines internal helper functions
mimori slice compose.ts#L100-L180                     # exact GitHub-style coordinate range
mimori slice compose.ts:125                           # coordinate peek window
mimori slice src/engine/core.py                       # whole file (adaptive: <=250 lines rendered completely)
mimori slice src/engine/core.py --offset 50 --lines 20 # manual window pagination
mimori slice token.py:verify_jwt                      # fuzzy path/symbol match

# →  # Slice: src/auth/token.py (symbol: verify_jwt)
#    - Coordinates: lines 42–78 (210 total)
#    - Ancestors (In-Degree 3): `api`, `middleware`
#    - Dependencies (Out-Degree 2): `jose`, `config`
#    - Contract: `verify_jwt(token: str) -> Claims`
#    ```python
#       42: def verify_jwt(token: str) -> Claims:
```

**Tree traversal (Zero Pollution, MUST):**
`Canopy` `mimori map --focus <target>` → `Contract` inspect types at boundary → `1-Hop Slice` `mimori slice` → `Leaf` `file.py#L40-L75`. Whole-file `read` >100 lines **NEVER** — that's the rule.

---

## Installation

**One-line:**

```bash
curl -fsSL https://raw.githubusercontent.com/fusuyfusuy/mimori/main/install.sh | bash
# → ~/.local/bin/mimori, verifies, checks PATH
```

**Manual (zero deps, Python 3.10+ only):**

```bash
curl -fsSL https://raw.githubusercontent.com/fusuyfusuy/mimori/main/mimori -o ~/.local/bin/mimori
chmod +x ~/.local/bin/mimori
mimori --version  # 1.5.0
mimori --test     # self-test + fixtures
```

No `pip`, no `npm`, no daemon. One file, ~150KB. **Zero deps to run.**

**Optional — tiered AST engines (better with, works without):**

```bash
# Tier 1 — tree-sitter (best fidelity, if Python module available)
pip install tree_sitter tree_sitter_python tree_sitter_typescript  # or: pip install tree_sitter_languages
# Tier 2 — pure stdlib fallback (always works, no install) — mimori auto-degrades
MIMORI_DEBUG=1 mimori map --stdout   # surfaces engine diagnostics to stderr
```
> `mimori` auto-detects at runtime: `tree_sitter` Python module → stdlib regex. A missing tier never breaks — just ~10–20% less precise on TS/JS/Go/Rust symbol boundaries. See [Tiered AST Engines](#tiered-ast-engines--better-with-works-without).

---

## Quickstart

```bash
mimori init                                           # scaffold .mimori/ (auto git init)
mimori dump --file                                    # warmup → in-scope .mimori/.cache/context.md, <0.2s
mimori dump --seed "stack deploy"                     # Topic-Sensitive PageRank elevated dump
mimori dump --focus "auth,api"                        # focused subsystem
mimori dump --scope "packages/server,packages/common" # monorepo multi-scope isolation (<0.5s)

mimori map --stdout --seed "compose"                  # Topic-Sensitive PageRank elevation
mimori map --stdout --no-tests --kind backend         # noise filtering: strip tests & UI components
mimori map --stdout --focus "auth"                    # ranked symbols + importers
mimori map --stdout --scope "src/services"            # scoped symbol map
mimori map --stdout --format json                     # machine-readable (with seed_match)

mimori slice src/auth/token.py:verify_jwt             # 1-hop slice (full symbol body by default)
mimori slice compose.ts:createCommand --follow-local  # inline private helpers in same file
mimori slice compose.ts#L100-L180                     # exact coordinate range
mimori slice src/engine/core.py                       # whole file (adaptive: <=250 lines)
mimori slice src/engine/core.py --offset 50 --lines 20 # window pagination
mimori slice token.py:verify_jwt --scope "src/auth"

mimori todo add "Refactor token cache" --prio high --tag perf
mimori todo add "Implement query engine" --start
mimori todo                                           # list [/] + [ ]
mimori todo done 1
mimori idea add "Distributed AST index"
mimori idea promote 1

mimori debt                                           # list # ponytail: markers
mimori debt list --scope "packages/server"            # scoped debt audit
mimori debt sync                                      # sync to memory.md ## KNOWN DEBT
mimori debt check                                     # CI gate: exit 0 clean, 1 broken

mimori log --action "add-auth" --summary "Added JWT middleware" --files "auth.py,server.py"
mimori history --limit 5
mimori clean && mimori clean --all
```

---

## How it works (boring, correct)

**Not a vector DB. Deterministic computation.**

* **Git is truth.** `git ls-files --cached --others --exclude-standard` — no hand-rolled `.gitignore` parser (old one got negations, trailing globs, scoped patterns all wrong). `os.walk` fallback only for non-git. Supports multi-scope commas (`--scope a,b`) and normalized internal absolute paths.
* **Topic-Sensitive PageRank, not blind keyword search.** Literal token search (`rg -l -i -F`) seeds a personalized teleportation vector ($v[i] = 1/|S|$). Rank propagates across the AST call graph to elevate polymorphic subtypes and execution engines (e.g. `compose.ts` for `"stack deploy"`) that lexical path filters miss.
* **Pre-Graph Noise Filtering.** `--no-tests` prunes test suites/files and `--kind backend|frontend` separates backend logic from React/Vue UI trees *before* graph calculation, preventing tests and UI components from monopolizing in-degrees.
* **Zero-Daemon SQLite AST Delta Cache.** Symbol graphs and file metadata cache to `.mimori/.cache/ast.db` with WAL mode. Re-indexes only touched files based on `(mtime_ns, size)`, achieving <0.1s incremental updates on massive monorepos.
* **PageRank, not heuristics.** Flat `array('i')`/`array('d')` power iteration, dangling-node correction, `delta<1e-7` early exit. Microsecond convergence. Ranks by `in-degree × 4 + PageRank × N × 4 + min(90d churn, 10) + entry points (6) + symbols (1) + seed bonus (8)`. Top of map is `components/ui/button.tsx ←39`, `lib/utils.ts ←29` — not alphabetical accident.
* **Polyglot, honest degradation.** Python `ast` → [Tiered AST Engines](#tiered-ast-engines--better-with-works-without) (`tree-sitter` → stdlib regex → names-only). Never lies about what it parsed. `MIMORI_FORCE_REGEX` escape hatch. `tsconfig` aliases (`@/…` 401:26 in titirek), `go.mod`, `Cargo.toml`/`mod.rs`/`crate::*` — ignored languages get symbols/churn ranking, not fake edges.
* **Budget honesty.** `dump` spends one total budget (`default 24K`, `large 60K`, `immense 200K`, `unlimited`) as `memory > decisions > tasks > map > activity`. No silent caps — every collapse prints `Detailed N of M` or `N completed hidden`. `map` is unlimited by default (disk, not context); `dump` is budgeted (context).
* **No silent failures.** `get_git_branch` → `None` outside git, `detached@<sha>` on detached HEAD (old code faked `main`). `MIMORI_DEBUG=1` surfaces swallowed exceptions (`tree-sitter`, `pagerank`, `entry_points`). Stale refs in `memory.md`/`tasks.md` scanned via `scan_stale_references`, shown as decay notices in `dump`.

---

## Tiered AST Engines — Better With, Works Without

`mimori` never requires external dependencies. It probes tiers at runtime, best → fallback, and **never lies** about what it parsed:

| Tier | Engine | Install | What you get | Fallback |
|---|---|---|---|---|
| **1** | `tree-sitter` | `pip install tree_sitter tree_sitter_python ...` | Full AST fidelity: precise class/method boundaries, Go/Rust/C++ structs, TSX/JSX | if missing → Tier 2 |
| **2** | **stdlib** | _(nothing)_ | `ast` for Python + regex heuristics for others | always works |

* **Detection:** `mimori` checks `import tree_sitter` at startup. No config.
* **Why no ast-grep tier?** v1.4 shipped one. Measured on 600 TS files it returned zero symbols on
  ast-grep 0.45.2 (its `--pattern '$$$'` probe matches nothing) while costing ~6 ms of subprocess
  per file — 3.19 s vs 0.46 s CPU for byte-identical output. Removed rather than left as a
  documented capability that does not execute.
* **Honesty:** `MIMORI_DEBUG=1 mimori map` prints degrade diagnostics to stderr if optional tiers are missing.
* **Coverage:** Tested on Python, TS/JS, Go, Rust, Ruby, C, C++ across `titirek` (162 files) and `opencode` (6,515 files). Unsupported languages → symbols + churn ranking, no fake edges.
* **Why tiered?** v1.0 was pure regex — correct ranking on Python but missed 94% of `@/` imports in Next.js. Adding tiers fixed `464 vs 26 edges` (titirek) without ever breaking the one-file, zero-deps promise.

---

## vs alternatives

| Approach | Strengths | Why mimori wins |
|---|---|---|
| **Vector DB / embeddings** | Semantic search | Probabilistic, drifts, needs daemon + RAM, re-index on branch switch, no line numbers |
| **LSP / `lsp references`** | Precise call graph | Requires running daemon, per-language setup, no ranking, no budget |
| **Raw `grep`/`glob`** | Universal | 80+ hits, no ranking, misses path aliases (`@/lib` 94% of edges), loads whole files |
| **Aider's repo map** | PageRank idea | Inspiration for mimori; mimori adds budget, decay, tasks, debt, PageRank vectorization |

Every number above is reproducible: run `mimori dump --file` + `mimori slice` on your own repo and compare against the [`artifacts/`](artifacts/) writeups.

## Agent harness integration

Designed for **Claude Code, Antigravity, Pi, OpenCode, Aider** — any harness that respects `AGENTS.md`.

**Install skill:**

```bash
cp SKILL.md ~/.claude/skills/mimori/SKILL.md        # Claude Code
cp SKILL.md ~/.gemini/antigravity-cli/skills/mimori/SKILL.md  # Antigravity
cp SKILL.md ~/.pi/agent/skills/mimori/SKILL.md      # Pi
```

**Drop into `AGENTS.md`:**

```markdown
## Project Memory & Lifecycle Protocol (mimori)

### 1. Explore -> Plan -> Approve -> Execute -> Verify
- **Explore — Tree Traversal (Zero Pollution, MUST)**:
  1. **Canopy**: `mimori map --stdout [--scope "<dirs>"] [--seed "<term>"] [--no-tests] [--kind backend|frontend] [--focus "<target>"]` for PageRank & in-degree ranking. Use `--seed` for topic-sensitive boost on polymorphic handlers; `--no-tests` and `--kind backend` to purge test/UI noise.
  2. **Contract**: inspect public types/interfaces at boundary, prune rest.
  3. **1-Hop Slice**: `mimori slice <file>[:<symbol>|#L<start>-L<end>] [--follow-local]` for callers+deps+slice. Full symbol body rendered by default; files <= 250 lines rendered completely. Use `--follow-local` (`-f`) to inline private helper callees.
  4. **Leaf**: exact `file.py#L40-L75`. Whole-file reads >100 lines NEVER — `mimori slice` before `read`.
- **Plan**: Track multi-step tasks in `mimori todo` (e.g. `mimori todo add "Refactor parser" --start`).
- **Approve**: Multi-file/API/dependency changes require plan review (`RequestFeedback=true`).
- **Execute**: Shortest working diff. Mark shortcuts `# ponytail: <what> <- <ceiling> -> <trigger>`.
- **Verify & Gate**: Cite slices/maps used (`slice X:42-90`, `map --focus Y`); `mimori debt check` (exit 0); one assert check.
- **Log**: `mimori log --action <act> --summary <caveman> --files <f1,f2>` (<160 chars).

### 2. Session Warmup & Hygiene
- **Warmup**: `mimori dump --file [--scope "<dirs>"] [--seed "<term>"] [--no-tests]` at session start (in-scope `.mimori/.cache/context.md`). Never read `.mimori/repo_map.md` directly.
- **Decay Pruning**: Remove stale refs reported in `dump` from `.mimori/memory.md`.
- **Subagent Kickoff**: `mimori dump --file [--scope "<dir>"] --focus "<area>"` + `mimori slice <target> [--follow-local]`.

### 3. Debt Governance & Writing Style
- **Writing Style**: Caveman (drop filler, keep code/paths/numbers/negations) in `memory.md`/ADRs.
- **Debt Sync & CI Gate**: `mimori debt sync` → `memory.md ## KNOWN DEBT`; `mimori debt check [--scope "<dir>"]` CI gate.
```

**Lifecycle matrix:**

| Phase | Action | Command | Impact |
|---|---|---|---|
| Session Start | Warmup + decay check | `mimori dump --file` | ~12–24KB cached, saves 50K |
| Domain Search | Topic PageRank | `mimori map --seed "concept" --no-tests` | Elevates polymorphic engines |
| Exploration | Ranked symbols | `mimori map --focus "auth"` | Top hubs only |
| Leaf | 1-hop slice + callee inlining | `mimori slice <file>[:<sym>] -f` | 120 vs 3,000 tokens |
| Planning | Track work | `mimori todo add "<task>" --start` | `.mimori/tasks.md` |
| Subagent | Scoped kickoff | `mimori dump --focus "<target>"` + `slice` | Minimal window |
| Verify | Debt gate | `mimori debt check [--scope <dir>]` | exit 0/1 |
| Journal | Log action | `mimori log ...` | `.mimori/activity.jsonl` |

---

## Repository layout

```
.mimori/
├── memory.md         # invariants, gotchas, ## Flagged ambiguities, ## KNOWN DEBT
├── decisions.md      # ADRs: Context → Decision → Consequences
├── tasks.md          # [ ], [/], [x], [?] + Ideas
├── repo_map.md       # full AST map (unlimited, for humans; dump is budgeted)
├── activity.jsonl    # append-only action log (whole .mimori/ is gitignored by default)
├── .gitignore        # auto-shields .cache/, .locks/, and *.tmp*
├── .locks/           # multi-process advisory file locks (*.lock)
└── .cache/           # in-scope execution cache
    ├── context.md    # deterministic context snapshot from dump --file
    └── ast.db        # zero-daemon SQLite AST delta cache (WAL mode)

mimori               # single-file CLI, stdlib only
SKILL.md             # agent skill (MIMORI(1) agent man page)
test_mimori.py       # 18-suite integration test & regression verification
install.sh           # curl → ~/.local/bin/mimori
```

---

## Philosophy

**Three Forests (三森, *mimori*)** — Memory (invariants), Structure (PageRank graph), Action (ADRs/tasks/logs).

**Ponytail:** Lazy senior dev — *the best code is code never written.* YAGNI → reuse → stdlib → platform → dependency → one-liner → minimal diff. Deletion over addition, boring over clever. Mark deliberate shortcuts `ponytail: <what> <- <ceiling> -> <trigger>` — `mimori debt` keeps the ledger honest.

**Caveman:** Drop articles/filler, keep code/paths/numbers/`not`/`never`. Memory is for agents, not prose.

**Smallest working slice first, then layer.** No stopgaps, no backward-compat shims.

---

## Acknowledgements

Standing on shoulders of giants:

* **Ponytail (Dietrich Gebert)** — Lazy Senior Dev ladder, `# ponytail:` ledger (adapted, not copied — 4 divergences found dogfooding on `titirek` 162 files)
* **Caveman (Julius Brussee)** — compression rules (articles dropped, `not` never dropped)
* **Aider (Paul Gauthier)** — PageRank on definition/reference graphs (mimori vectorizes it)
* **Pi / Mario Zechner** — minimal loop, extensions, session publishing
* **Fusuycorp/titirek** — 162-file Next.js/PocketBase dogfooding found `@/alias` (401:26), barrel re-exports, all ranking bugs

---

## License

[MIT License](LICENSE) © 2026 Yusuf Akcakaya — `fusuyfusuy/mimori`
