# With vs Without mimori — OpenCode Repo Exploration (6,515 files / 680K LOC)

| Aspect | With `mimori` | Without `mimori` |
|:---|:---|:---|
| **Orientation & Discovery** | `mimori dump --file` generates a ranked index of 6.5k files in seconds; `mimori slice` directly targets key symbols with 1-hop lineages | `find` / `grep` queries returning 80+ file hits across desktop app, web, and TUI; manual triage across fragmented UI layers |
| **Context Extraction** | `mimori slice <file>:<sym>` returns bounded, line-anchored coordinates, exact caller in-degree, downstream deps, and contract (<100 lines / ~400 tokens per slice) | Full file reads of 480-560 line component trees (>3,500 to 5,500 tokens per file) |
| **Context Window Consumption** | **~880 tokens** total ingested for cross-surface exploration | **~10,900 tokens** (12× context bloat) from reading full components, stores, and modules |
| **Compounding Turn Cost** | Minimal ongoing cost; subsequent turns stay lean and fast (<1K extra input context) | Ingested component code stays in conversational history, adding ~11K tokens to *every* future user turn |
| **Accuracy & Navigation** | Deterministic symbol bounds and import graph ancestry; precise `file:line` references into the target components | Heuristic guesswork on line boundaries; potential missed context across multi-package boundaries |
| **Tool Calls** | 3 deterministic symbol slices (`mimori slice`) | 6–10 tool calls (multiple recursive file reads, greps, and offset paginations) |

*Generated 2026-08-28.*
