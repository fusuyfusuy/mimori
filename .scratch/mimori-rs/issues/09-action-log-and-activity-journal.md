# 09: Action Log & Activity Journal

**What to build:** Implement `mimori-rs log` action journal command to record discrete high-signal actions (<160 chars) into an append-only `.mimori/activity.jsonl` log file. Enrich `mimori-rs dump` to ingest and present recent activity alongside the architectural map.

**Blocked by:** 08: Agent Skill Packaging & End-to-End Test Suite (clean)

**Status:** resolved

- [x] Add `LogArgs` to `src/cli/args.rs` with `--action`, `--summary`, and optional `--files`.
- [x] Implement `ActivityRecord` model and append/read utilities in `src/workspace/journal.rs`.
- [x] Wire `Commands::Log` in `src/main.rs` with both Markdown confirmation and `--json` serialization.
- [x] Update `mimori-rs dump` in `src/main.rs` to ingest recent entries from `.mimori/activity.jsonl` into the output context.
- [x] Implement integration tests in `tests/cli_log.rs` covering logging, `.mimori/activity.jsonl` persistence, and `dump` rollup.
- [x] Update `.agents/skills/mimori-rs/SKILL.md` with `log` subcommand documentation and examples.
