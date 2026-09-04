# 05: In-Degree PageRank Centrality & Structural Code Map (map)

**What to build:** Compute in-degree PageRank centrality over the dependency graph to evaluate architectural importance, and implement the `map` command. When an AI agent runs `mimori-rs map`, the CLI outputs a token-dense, centrality-ranked structural skeleton of top-level modules and backbone symbols, allowing the agent to grasp the architecture within a compact token budget. Also ranks ambiguous `find` matches by centrality.

**Blocked by:** 04: Cross-File Dependency Resolution (up & down)

**Status:** resolved

- [x] Implement power iteration PageRank algorithm ($d = 0.85$, 25 iterations) computing in-degree centrality score for every symbol.
- [x] `mimori-rs map` generates a hierarchical repository map with top symbols ranked by centrality.
- [x] Support `--scope <dir>` to restrict map generation to a specific directory or module.
- [x] Support `--focus <target>` to emphasize symbols connected to a focal coordinate.
- [x] Rank results in `mimori-rs find` and symbol disambiguation lists by centrality score descending.
- [x] CLI integration tests verify `map` formatting and centrality ranking on fixture projects.
