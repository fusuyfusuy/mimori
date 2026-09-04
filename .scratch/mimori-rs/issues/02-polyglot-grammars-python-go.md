# 02: Polyglot Grammar Extension (Python & Go)

**What to build:** Extend the AST parsing and slice extraction capabilities to Python and Go source files. When an AI agent or developer runs `mimori-rs slice` on a `.py` or `.go` file targeting classes, functions, methods, structs, or interfaces, the CLI extracts the exact symbol body and coordinate ranges.

**Blocked by:** 01: Project Skeleton, CLI Scaffold & Single-File Slicing (Rust & TS)

**Status:** resolved

- [x] Statically embed Tree-sitter parsers for Python (`tree-sitter-python`) and Go (`tree-sitter-go`).
- [x] Implement AST symbol extraction queries for Python (classes, defs, async defs, decorated functions, module constants).
- [x] Implement AST symbol extraction queries for Go (structs, interfaces, functions, methods with receiver types).
- [x] `mimori-rs slice <file.py:symbol>` and `mimori-rs slice <file.go:symbol>` extract full symbol body and line coordinates.
- [x] CLI integration tests verify slice extraction on Python and Go sample files.
