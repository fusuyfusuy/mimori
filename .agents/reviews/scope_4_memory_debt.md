---
scope: "Project Memory & Debt"
score: 7.2
status: "MODERATE"
critical_findings: 3
invariant_breaches:
  - "Non-destructive sync: sync_debt purges manual debt items lacking 'accepted' prefix (debt.rs:324-331)"
  - "Zero untracked debt: check_debt passes even if in-code markers are unsynced to memory.md (debt.rs:262-310)"
  - "Safe persistence: lack of atomic file writes and concurrency locking on memory.md (ledger.rs:218, debt.rs:353)"
---

# Project Memory & Technical Debt Audit Report

## 1. Executive Overview
A rigorous audit was conducted across `src/memory/` (`mod.rs`, `ledger.rs`, `debt.rs`, `dump.rs`) and `.agents/` (`memory.md`, `decisions.md`).
While `mimori` implements fast parallel in-code debt discovery and strict M2M lint formatting, it exhibits critical vulnerabilities in sync data retention, file I/O safety, code-to-ledger parity enforcement, and Turn-0 context completeness.

## 2. Dimension 1: Correctness & Marker Parsing
- **Silent Deletion of Manual Debt** (`src/memory/debt.rs:324-331`, `src/memory/ledger.rs:412-413`): `sync_debt` retains manual debt only if `item.is_accepted` is true (`what` starts with `"accepted"` or `"[accepted]"`). Valid manual entries conforming to `DEBT_SCHEMA: "- <what> <- <why> -> <trigger>"` lacking this prefix are wiped out on `sync`.
- **Divergent Accounting Between Check and Lint** (`src/memory/debt.rs:267-271`, `src/memory/ledger.rs:265`): `check_debt` counts only manual items where `d.is_accepted == true`, while `MemoryLedger::lint` counts all `raw_debt_lines`. A file with 5 manual items reports 5 in `lint` but 0 in `check_debt`.
- **Comment Prefix Fragility** (`src/memory/debt.rs:33-38`, `84-91`): The scanner checks exact literal prefixes (`"// ponytail:"`, `"# ponytail:"`). Extra whitespace (`//  ponytail:`), tabs, or block comments (` * ponytail:`) fail detection entirely.
- **Line-Stateless Lexing & Raw Strings** (`src/memory/debt.rs:40-98`): Scans line-by-line without multi-line token state; false positives occur inside multiline string literals. Rust raw strings (`r#"..."#`) with quotes break `in_double_quote` tracking and skip valid trailing comments.
- **Silent Truncation on Ceiling Breach** (`src/memory/debt.rs:345-350`): When total debt exceeds 30, `sync_debt` truncates lines and exits 0 with a warning instead of halting, dropping legitimate items from the ledger.

## 3. Dimension 2: Robustness & Concurrency
- **Non-Atomic I/O and Concurrency Race** (`src/memory/ledger.rs:218-222`, `src/memory/debt.rs:351-354`): Memory updates execute via bare `fs::write` without file locking (`flock`) or atomic rename (`tempfile + rename`). Concurrent agent invocations or aborted runs risk lost updates or torn files.
- **Section Overwrite Hazard** (`src/memory/debt.rs:383-402`): `replace_debt_section` checks `heading.contains("debt")`. Any section containing "debt" (e.g. `## Technical Debt Discussions`) matches and gets replaced. Furthermore, headings with `#` or `###` do not reset `in_debt_section`.
- **Comment Stripping in Debt Section** (`src/memory/debt.rs:388-391`): Replaces all user comments in `## KNOWN DEBT` with a single hardcoded comment line `# Deliberate gaps get ledger lines...`.
- **Unbounded Pattern Deletion** (`src/memory/ledger.rs:280-319`): `resolve(pattern)` deletes all lines matching `contains(&pattern_lower)`, risking accidental multi-item mass deletion on short substrings.

## 4. Dimension 3: Context Packing & Turn-0 Budgeting
- **Active Epics Omitted** (`src/memory/dump.rs:26-59`): `MemoryLedger::from_str` parses `pub epics: String`, but `generate_dump` completely ignores `l.epics`, depriving agents of top-level project scope in Turn-0 snapshots.
- **Architectural Decisions (ADRs) Ignored** (`src/memory/dump.rs:1-119`): `.agents/decisions.md` is omitted from Turn-0 dumps.
- **Budget Saturation Clamping Waste** (`src/memory/dump.rs:63-73`): When vocab and debt exceed the budget, `max_map_chars` saturates to 0, yet `.clamp(3, 40)` still generates a 3-symbol map that is immediately discarded during truncation.
- **Token Inefficiency** (`src/memory/dump.rs:121-142`): Employs decorative emojis (`📁`, `🔹`) and verbose markdown formatting, squandering token budget.
- **Hardcoded Header Budget Reserve** (`src/memory/dump.rs:62`, `104`): Hardcodes 150 chars for a ~55-char header, needlessly wasting ~24 tokens.

## 5. Dimension 4: Invariant Compliance
- **Missing Code-to-Ledger Parity Check** (`src/memory/debt.rs:262-310`): Neither `check_debt` nor `lint` verifies that in-code markers match `memory.md`. Developers can add ponytail markers that remain completely untracked in `memory.md` without failing CI.
- **Format Preservation**: Compromised by comment erasure and non-accepted item deletion during `sync_debt`.

## 6. Actionable Remediation Plan
1. **Preserve All Valid Manual Debt**: Remove the `item.is_accepted` filter in `sync_debt` and `check_debt`; track source origin or preserve all valid debt lines.
2. **Implement Atomic File Writes**: Use temporary files and atomic rename with advisory locking for all mutations to `.agents/memory.md`.
3. **Enforce Parity in `check_debt`**: Fail `check_debt` if in-code markers do not match entries in `.agents/memory.md`.
4. **Include Active Epics in Dump**: Inject `## ACTIVE EPICS & SCALE` into `generate_dump` and strip decorative emojis to optimize token density.
