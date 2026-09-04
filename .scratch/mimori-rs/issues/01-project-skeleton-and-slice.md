# 01: Project Skeleton, CLI Scaffold & Single-File Slicing (Rust & TS)

**What to build:** A working Rust CLI executable that parses Rust and TypeScript source files using statically bundled Tree-sitter grammars and provides the `slice` command. When an AI agent or developer invokes `mimori-rs slice <path/to/file:symbol>` or `mimori-rs slice <path/to/file:#Lstart-end>`, the CLI returns a token-dense Markdown view with the symbol's exact coordinate range, signature, and body (up to 250 lines), or structured JSON when `--json` is supplied.

**Blocked by:** None (can start immediately)

**Status:** resolved

- [x] Cargo workspace/binary initialized with `clap` CLI parser accepting commands (`slice`, `init`, `--json`).
- [x] Statically embedded Tree-sitter parsers for Rust (`tree-sitter-rust`) and TypeScript/JavaScript (`tree-sitter-typescript`).
- [x] AST symbol extractor queries functions, structs, traits, enums, type aliases, and methods with accurate line coordinates.
- [x] `mimori-rs slice <file:symbol>` returns exact symbol body, coordinate range, and signature formatted as compact Markdown.
- [x] `mimori-rs slice <file:#L10-50>` extracts specified line slices with line-numbered Markdown output.
- [x] Passing `--json` outputs structured JSON containing coordinates, signature, and source body.
- [x] End-to-end CLI integration tests (`assert_cmd`) verify successful execution on Rust and TypeScript sample files.
