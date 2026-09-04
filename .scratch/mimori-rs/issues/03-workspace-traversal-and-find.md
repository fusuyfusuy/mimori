# 03: Workspace Traversal, Ignore Filtering & Symbol Search (find)

**What to build:** Parallel repository-wide file discovery respecting `.gitignore`, `.ignore`, and default ignore patterns (`node_modules`, `target`, `vendor`, `.git`) via the `ignore` crate, and the `find` command. When an AI agent runs `mimori-rs find <pattern> [-s|-f]`, the CLI scans the workspace and returns matching symbols or files with their coordinates.

**Blocked by:** 01: Project Skeleton, CLI Scaffold & Single-File Slicing (Rust & TS)

**Status:** resolved

- [x] Repository discovery walker using `ignore` crate and `rayon` for parallel multi-threaded file walking.
- [x] Built-in automatic ignores for build artifacts (`target`, `node_modules`, `.git`, `dist`, `vendor`, `.mimori`).
- [x] `mimori-rs find <pattern>` searches all parsed symbols across the repository, returning symbol names, kinds, files, and line coordinates.
- [x] Support `-s` (symbols only) and `-f` (files only) filtering flags.
- [x] Markdown output lists matching coordinates concisely; `--json` outputs structured array of matches.
- [x] CLI integration tests verify symbol and file search across a multi-file fixture project.
