# 4. Pure Code Dissection Engine (Retire Action Journal & Snapshot Dumps)

- **Status**: Amended by [ADR-0006](0006-reclaiming-project-memory-substrate.md)
- **Date**: 2026-09-13

We decided to strip out activity journaling (`mimori log`), workspace action history (`.mimori/activity.jsonl`), and context snapshot dumping (`mimori dump`), keeping `mimori` focused strictly on AST parsing, codebase crawling, and code dissection (`slice`, `find`, `up`, `down`, `uses`, `blast`, `map`, `missing`, `doctor`).

Action logging and decision tracking belong to higher-level agent coordination layers and project management substrates (such as issue trackers and version control commit histories), not to the low-level code dissection binary. Eliminating file-append mutations, journal rotation logic, and snapshot markdown assembly keeps the CLI minimal, avoids state bloat inside `.mimori/`, and enforces Unix single-responsibility principles.

## Addendum (2026-09-14, v2.4.0; amended by ADR-0007)
Amended by [ADR-0006](0006-reclaiming-project-memory-substrate.md) and [ADR-0007](0007-typed-ast-stratified-ranking-and-jit-memory.md) to restore project memory (`.agents/memory.md`), Tree-sitter comment `mimodept:` debt synchronization (`mimori debt`), JIT scoped memory, and token-budgeted context dumping (`mimori dump`), while preserving the ephemeral isolation of `.mimori/`.
