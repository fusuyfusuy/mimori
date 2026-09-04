# 08: Agent Skill Packaging & End-to-End Test Suite

**What to build:** Package the tool with an AI agent skill definition (`SKILL.md`) and a comprehensive end-to-end integration test suite that verifies the entire `mimori-rs` workflow across a realistic multi-language, multi-module testbed repository.

**Blocked by:** 07: SQLite Storage, Instant Queries & Incremental Caching (clean)

**Status:** resolved

- [x] Agent skill definition file (`.agents/skills/mimori-rs/SKILL.md`) with instructions, command references, and token-saving patterns for AI agents.
- [x] Multi-language fixture testbed containing Rust, TypeScript, Python, and Go modules with circular dependencies, trait implementations, and deep call chains.
- [x] End-to-end integration test validating the entire CLI workflow (`init`, `map`, `find`, `slice`, `up`, `down`, `blast`, `clean`).
- [x] Verification of token density and format conformance across Markdown and JSON output modes.
