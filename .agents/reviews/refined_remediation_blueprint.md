# Refined Remediation Blueprint — mimori v2.4.8 (2026-10-12)

Mode: single-agent (subagent quota 429). Operator requested "fix all", so remediation was executed after adjudication. No test files were touched.

## Adjudication

| ID | Verdict | Evidence | Disposition |
|---|---|---|---|
| M1 debt sync data loss | REDIRECTED_TO_SOURCE | Non-`accepted` lines are pruned by design (ledger header: "Deliberate gaps ... - accepted"); defect = pruning was silent | FIXED: output lists `PRUNED N ledger line(s)` ([debt.rs](file:///home/devhax/projects/fusuyfusuy/mimori/src/memory/debt.rs)) |
| I1 MCP unbounded line | CONFIRMED_ROOT_CAUSE | 200 MB line -> 202,752 KB RSS | FIXED: 4 MiB `read_bounded_line`, -32600; RSS 10,192 KB ([mcp/mod.rs](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/mod.rs)) |
| G1 unresolved calls | REDIRECTED_TO_SOURCE | `coord.name()` cross-file = intentional member-call precision rule (graph test `member_calls_resolve_same_file_only`). Real root cause: Rust parser ignores calls inside macro token trees (`format!(..self.f()..)`) | FIXED: `macro_token_call` in [rust.rs](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs); `PARSER_VERSION` 9->10 ([db.rs](file:///home/devhax/projects/fusuyfusuy/mimori/src/storage/db.rs)) |
| M2 check ignores unsynced | FALSIFIED_DISMISSED | README.md:15,118 + ADR-0006:25 define `check` as triggers + ceiling only; tests/cli_debt.rs asserts it. Implemented then reverted | Dismissed |
| M3 ceiling truncation | FALSIFIED_DISMISSED | `DEBT_SYNC_WARN` emitted; `debt check` fails on ceiling | Dismissed |
| G2 ambiguous drops unsurfaced | Deferred | Stats already show count in `doctor`; per-query note is a feature | Not done |
| I2 MCP lacks uses/missing/doctor | Deferred | Feature addition, public interface change | Not done |
| S3 SKILL.md installed copy drift | Out of repo | `~/configs/agents-config/...` outside workspace | Not done |

## Two-Phase Proof (C(S_fail) != 0 -> C(S_fail+diff) == 0)

- G1: `mimori down ...SliceResult::render_markdown` 0 callees -> 6; `up callers_markdown` 0 -> 2 callers; doctor isolated 138 -> 111, resolved 775 -> 879.
- I1: RSS 202,752 KB -> 10,192 KB; server answers -32600 then continues (follow-up `ping` OK).
- M1: sync of hand-written line: silent (`manual: 0`) -> `PRUNED 1 ledger line(s) ...`.
- Gates: `cargo build --release`, `cargo clippy --all-targets`, `cargo test` exit 0 (all suites); `mimori memory lint` and `mimori debt check` exit 0.
- `cargo fmt --check` diffs only in untouched lines (debt.rs:174, ledger.rs:82) — pre-existing.

## Residuals
Unbounded `mpsc::channel` in MCP (a slow handler can queue many <=4 MiB frames); Parsers and Storage scopes still un-reaudited.
