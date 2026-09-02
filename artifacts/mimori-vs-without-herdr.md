# With vs Without mimori — Herdr Agent Integration Research (A/B run, 2026-08-31)

Task given to both agents (identical prompt, same repo, same session minute):
*"this is the repo of herdr, a terminal manager which we use, command-code is not recognized as an agent and not supported, find out how to integrate to herdr"*

Both runs executed **in parallel** on model `z-ai/glm-5.3-flash` via Command Code. All numbers below are measured from the session transcripts on disk
(`~/.commandcode/projects/home-devhax-projects-clones-herdr/`), not estimated. Timestamps UTC.

## Run identification

| | WITHOUT mimori (session `10a66c0d`) | WITH mimori (session `e37d5690`) |
|:---|:---|:---|
| First user message | 20:21:58 | 20:21:25 |
| Approach | 2 parallel `explore` subagents + main agent | Loaded `mimori` skill, `mimori dump --file` warmup, direct reads/slices, logged via `mimori log` |
| Repo file access | Main agent: 0 direct repo reads during research (all delegated to subagents); later 2 `read_file` + 3 `read_directory` for report writing | All direct: 5 `read_file`, 7 `grep`, 13 `shell_command` |

## Tokens (main agent, 24–25 API calls each)

| Metric | WITHOUT mimori | WITH mimori |
|:---|---:|---:|
| Input tokens | 1,104,136 | 1,205,585 |
| Output tokens | 11,563 | 13,347 |
| Cache read tokens | 859,456 | 1,003,072 |
| Cache write tokens | 0 | 0 |
| **Total tokens** | **1,975,155** | **2,222,004** |
| **Cost (main agent)** | **$0.1972** | **$0.2176** |

Subagent tokens (WITHOUT mimori only; WITH run used no subagents):

| Subagent | Total tokens | Tool calls | Turns | Wall time |
|:---|---:|---:|---:|---:|
| A (thorough: registration/integration sweep) | 2,925,820 | 46 | 24 | 632 s |
| B (medium: manifest schema) | 328,101 | 12 | 6 | 144 s |
| **Combined** | **3,253,921** | **58** | 30 | ~632 s (ran in parallel) |

**Grand total (main + subagents):**

| | WITHOUT mimori | WITH mimori |
|:---|---:|---:|
| Tokens | **5,229,076** | **2,222,004** (2.35× less) |
| Cost | $0.1972 + subagent cost (not exposed by tool result) | **$0.2176** (fully accounted) |

## Time

| Metric | WITHOUT mimori | WITH mimori |
|:---|---:|---:|
| Conversation wall time (first msg → last captured msg) | 870 s (~14.5 min) — **still running** (report work + follow-up user requests) | 606 s (~10.1 min) — **finished** (final report written 20:31:18) |
| First substantive result | Integration plan presented at 20:24:22 (**~144 s** in, but 2 subagents still running until ~20:32) | Full integration surface reported at 20:25:50 (**~265 s** in), no background work pending |

## Tool calls

| | WITHOUT mimori | WITH mimori |
|:---|---:|---:|
| Main agent tool calls | 26 (2 agent, 12 shell, 2 glob, 2 write, 3 read_dir, 2 read, 1 grep, 1 ask, 1 todo) | 29 (1 skill, 13 shell, 5 read, 7 grep, 3 write) |
| Subagent tool calls | 58 | 0 |
| **Total** | **84** | **29** |

## Result

Both agents produced the same answer — command-code integration requires a herdr binary change with three tiers:

- `CommandCode` variant in `enum Agent` (`src/detect/mod.rs`) + bundled `src/detect/manifests/commandcode.toml` + `BUNDLED_MANIFESTS` registration
- Optional hook integration (`IntegrationTarget`, `src/integration/*`, resume via `commandcode --resume`)
- Docs rows + CHANGELOG

## Caveats

- WITHOUT-run totals were snapshotted mid-session; the run was still consuming tokens on report-revision turns. WITH-run numbers are the final state at 20:31:30.
- Subagent token totals include harness/system overhead as reported by the `agent` tool result; subagent dollar cost is not exposed.
- WITHOUT-run grand total mixes two accounting sources (transcript usage + subagent-reported totals).
- The WITH-agent filed its report under `mimori/.mimori/reports/` while the pre-existing convention in this repo is `artifacts/mimori-vs-without-*.md` — reports from both agents should be reconciled to one location.

*Generated 2026-08-31 by the WITHOUT-mimori agent (session `10a66c0d`), from on-disk transcript telemetry.*
