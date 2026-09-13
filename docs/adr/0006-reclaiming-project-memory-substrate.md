# 6. Reclaiming Project Memory Substrate & Ponytail Debt Engine

- **Status**: Accepted (Amends ADR-0004)
- **Date**: 2026-09-14
- **Deciders**: Mimori Core Team, Agent Substrate Working Group

## Context
ADR-0004 established `mimori` as a pure code dissection engine by stripping stateful action journaling (`mimori log`), workspace action history (`.mimori/activity.jsonl`), and context snapshot dumping (`mimori dump`). The intention of ADR-0004 was justified: eliminate unbounded file-append mutations, journal rotation logic, and state bloat inside `.mimori/` to respect Unix single-responsibility principles.

However, ADR-0004 over-corrected. By stripping all memory and debt management from `mimori` (*メモリ* — Japanese for *Memory*), the tool was divorced from its foundational namesake and primary purpose. Autonomous coding agents operating without a deterministic, high-speed memory and technical debt substrate were forced to rely on stochastic "prompt and pray" workflows. Debt tracking deteriorated into unvalidated markdown lists, resulting in attention degradation, silent debt accumulation, and lost domain gotchas across agent sessions.

Furthermore, autonomous agents require a zero-turn orientation mechanism (Turn-0 warm-up) that injects top-level architectural topology alongside hard-won empirical gotchas and active technical debt, strictly bounded within LLM token budgets.

## Decision
We decide to restore project memory, ponytail technical debt tracking, and Turn-0 context dumping directly into `mimori`, amending ADR-0004 under strict architectural boundaries and language contracts:

1. **Storage Boundaries**:
   - `.mimori/`: Strictly ephemeral throwaway cache. Houses the SQLite symbol index (`index.db`), WAL files, and temporary artifacts. It is completely safe to wipe at any moment via `mimori clean` and must never be committed to source control. `mimori init` automatically ensures `.mimori/` is present in `.gitignore`.
   - `.agents/`: 100% git-tracked living domain memory. Contains `memory.md` (active epics, 30-line ceiling known debt ledger, domain vocabulary, and empirical gotchas) and `decisions.md` (architectural decision records).

2. **In-Code Ponytail Technical Debt Engine**:
   - In-code comments formatted as `# ponytail: <what> <- <ceiling> -> <upgrade_trigger>` (as well as `//`, `/* ... */`, and `--` variants) serve as verifiable in-code ground truth for pragmatic deferrals.
   - `mimori debt list` uses multi-threaded Rayon scanning across the repository (respecting `.gitignore`) to discover and report all markers.
   - `mimori debt sync` deterministically synchronizes in-code markers into `.agents/memory.md` under `## KNOWN DEBT`, preserving operator-waived `- accepted ...` entries while automatically purging stale markers when comments are removed from code.
   - `mimori debt check` acts as an automated CI and agent proof gate asserting non-empty ceilings, valid triggers, and enforcement of the 30-line debt ceiling.

3. **Memory Ledger & CI Lint Gate**:
   - `mimori memory show` outputs structured, token-dense memory sections.
   - `mimori memory lint` validates that `## KNOWN DEBT` strictly conforms to the 3-tuple format (`- <what> <- <why> -> <trigger>`), enforces the maximum 30-line debt ceiling, and rejects completed checkboxes (`- [x]`) or strikethroughs (`~~`), guaranteeing that resolved debt is deleted rather than accumulated.
   - `mimori memory resolve <pattern>` surgically removes resolved debt items without disturbing adjacent formatting.

4. **Turn-0 Budget-Aware Context Dump**:
   - `mimori dump` builds a zero-turn prompt snapshot for fresh agent sessions, combining personalized or global PageRank architectural entry points with domain vocabulary, gotchas, and active debt, mathematically packed within a specified token budget (`--budget`, default 1500 tokens).

5. **Machine-to-Machine (M2M) Language Contract**:
   - All CLI and MCP telemetry, command outputs, status lines, and error messages for `memory`, `debt`, and `dump` must adhere strictly to token-dense Caveman / M2M syntax (zero articles, zero copulas, zero pronouns, zero narrative filler).
   - **Explicit Exception**: Architecture Decision Records (ADRs)—both in `.agents/decisions.md` and in `docs/adr/*.md`—remain written in rich, human-readable architectural prose for engineering operators.

## Consequences
- **Positive**:
  - `mimori` fulfills its namesake as a complete code-intelligence and memory substrate.
  - Eliminates stochastic prompt drift; fresh agents receive structured Turn-0 context with zero manual prompt assembly.
  - Technical debt is bound to a hard 30-line ceiling in CI, preventing debt backlog bloat.
  - Clear separation of concerns: `.mimori/` remains an ephemeral, rebuildable cache while `.agents/` remains the permanent, version-controlled domain truth.
- **Negative / Trade-offs**:
  - Adds parsing and regex scanning logic to `mimori`'s codebase, slightly expanding binary surface.
  - Requires developers and agents to follow the strict 3-tuple ponytail syntax for inline technical debt markers.
