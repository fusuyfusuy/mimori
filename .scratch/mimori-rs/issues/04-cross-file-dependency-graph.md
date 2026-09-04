# 04: Cross-File Dependency Resolution (up & down)

**What to build:** Build the cross-file dependency graph by extracting import statements, call sites, and type references across the workspace. When an AI agent runs `mimori-rs up <target>` (callers) or `mimori-rs down <target>` (callees), the CLI lists the upstream callers or downstream callees. Additionally, `mimori-rs slice` inlines immediate 1-hop callers and callees in its output.

**Blocked by:** 02: Polyglot Grammar Extension (Python & Go), 03: Workspace Traversal, Ignore Filtering & Symbol Search (find)

**Status:** resolved

- [x] AST extraction of call expressions, module imports, and type usages across Rust, TypeScript, Python, and Go.
- [x] Heuristic resolution engine linking call sites and references to target symbol definitions across files.
- [x] `mimori-rs up <target>` displays all upstream symbols that call or reference the target.
- [x] `mimori-rs down <target>` displays all downstream functions and types invoked by the target.
- [x] `mimori-rs slice <coordinate>` embeds the 1-hop list of callers and callees directly into the slice view.
- [x] Support `--follow-local` flag on `slice` to inline private local callee symbol bodies.
- [x] CLI integration tests verify `up`, `down`, and slice 1-hop caller/callee context across multi-file fixtures.
