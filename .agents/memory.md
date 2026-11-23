# Project Memory

## Active Epics & Scale
- Mission: Give agents a fast, token-disciplined way to learn the repo they are in (`dump`, `map`, `find`, `slice` + JIT `@scope:`/`@sym:` memory) and trace change impact/blast radii (`blast`, `diff`, `up`/`down`/`uses`, `missing`, `doctor`) — Rust CLI + MCP stdio, 4 languages (Rust, TS/JS, Python, Go), ~20ms warm map, zero daemons.
- Epic (v3.2.0 shipped): Typed AST symbol flags (`IS_TEST`, `IS_PUBLIC`, `IS_TRAIT_IMPL`, `IS_ATTR_ENTRY`), local receiver-type inference with explicit `blast` honesty warnings, module-stratified public-API map ranking, atomic SQLite cache with stat-gated MCP hot path, and scope-anchored `@scope:`/`@sym:` memory auto-injected into `slice`, `blast`, and `diff`.

## KNOWN DEBT (open only — one line per item, delete when done)
<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->

## Domain Vocabulary & Gotchas
- [@scope: src/storage/*] Content-hash invariant: `.mimori/index.db` invalidation is strictly FNV-1a content-hash driven (`PARSER_VERSION` bumps auto-rebuild); `quick_stat_fingerprint` uses POSIX `ino+size+mtime+ctime` as a warm-MCP fast path only.
- [@scope: src/graph/*] Edge precision invariant: Cross-file untyped `.method()` calls never resolve via unique-global; `blast` emits `unverified_member_calls` warning whenever unresolved member call sites share a method leaf name.
- [@scope: src/memory/*] Memory substrate invariant: `.mimori/` is ephemeral cache (gitignored); `.agents/memory.md` is git-tracked domain memory with soft 30-line debt ceiling (warn-only in CI) and Tree-sitter AST comment debt extraction.
- [@scope: src/workspace/*] Confinement invariant: Every CLI, MCP, manifest, and `--scope` path must pass lexical normalization + `confine_path` before touching disk (eliminates symlink escapes and `../` existence oracles).
- [@scope: SKILL.md, @scope: skills/mimori/SKILL.md] Dual-copy skill invariant: Keep `SKILL.md`, `skills/mimori/SKILL.md`, and `~/configs/agents-config/skills/mimori/SKILL.md` byte-identical when CLI/MCP contracts change.
