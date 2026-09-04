# 06: Transitive Ripple Impact / Blast Radius (blast)

**What to build:** Transitive impact calculation to evaluate which upstream callers and entry points will be affected if a symbol is modified. When an AI agent runs `mimori-rs blast <target> [-d <depth>]`, the CLI calculates and displays the upstream transitive closure tree up to the specified depth (default 3), reporting affected callers, entry points, and test files.

**Blocked by:** 04: Cross-File Dependency Resolution (up & down)

**Status:** resolved

- [x] Transitive upstream graph traversal algorithm computing the reachability closure from any target symbol.
- [x] Identification and highlighting of affected public entry points (main functions, API endpoints, exported symbols) and test suites.
- [x] `mimori-rs blast <target>` outputs a compact tree of affected callers and entry points in Markdown and `--json`.
- [x] Configurable traversal depth via `-d <depth>` flag.
- [x] CLI integration tests verify transitive blast radius accuracy on fixture projects.
