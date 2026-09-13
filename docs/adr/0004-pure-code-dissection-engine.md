# 4. Pure Code Dissection Engine (Retire Action Journal & Snapshot Dumps)

- **Status**: Amended by [ADR-0006](0006-reclaiming-project-memory-substrate.md)
- **Date**: 2026-09-13

We decided to strip out activity journaling (`mimori log`), workspace action history (`.mimori/activity.jsonl`), and context snapshot dumping (`mimori dump`), keeping `mimori` focused strictly on AST parsing, codebase crawling, and code dissection (`slice`, `find`, `up`, `down`, `uses`, `blast`, `map`, `missing`, `doctor`).

Action logging and decision tracking belong to higher-level agent coordination layers and project management substrates (such as issue trackers and version control commit histories), not to the low-level code dissection binary. Eliminating file-append mutations, journal rotation logic, and snapshot markdown assembly keeps the CLI minimal, avoids state bloat inside `.mimori/`, and enforces Unix single-responsibility principles.

## Addendum (2026-09-14, v2.4.0)
Amended by [ADR-0006](0006-reclaiming-project-memory-substrate.md) to restore project memory (`.agents/memory.md`), in-code ponytail debt synchronization (`mimori debt`), and token-budgeted context dumping (`mimori dump`), while preserving the ephemeral isolation of `.mimori/`.
