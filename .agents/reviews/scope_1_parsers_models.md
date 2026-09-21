---
scope: "parsers-and-models"
score: 9.3
status: "MINOR"
critical_findings: 0
invariant_breaches: []
remediated_issues:
  - "Go cross-package selector calls disambiguated from receiver methods via imported packages tracking (src/parser/go.rs#L26-L64, #L253-L262)"
  - "Python PEP 8 absolute imports no longer discarded; PYTHON_STDLIB whitelist segregates external vs workspace modules (src/parser/python.rs#L248-L335)"
  - "Slice line numbering formatting eliminates pipe character corruption in symbol and line-range slices (src/model/slice.rs#L271-L285)"
  - "Import collectors strictly enforce MAX_AST_DEPTH = 512 across Rust, TypeScript, Python, and Go (src/parser/*.rs)"
---

# Scope 1 Verification Audit Report: Ingestion, Parsers & Models

Post-remediation verification audit of polyglot Tree-sitter parsers (Rust, TypeScript, Python, Go) and core domain models (`Symbol`, `Coordinate`, `SliceResult`).

## 1. Executive Summary & Dimension Scores

| Dimension | Score | Assessment |
|---|:---:|---|
| **Correctness** | 9.4 | Go cross-package selector calls resolved, Python stdlib accurately partitioned, pipe-safe line numbering. |
| **Robustness** | 9.3 | AST recursion depth clamped (`MAX_AST_DEPTH = 512`) across symbol and import walkers; robust Windows coordinate parsing. |
| **Performance** | 9.0 | Fast Tree-sitter native C-bindings; zero daemon overhead; efficient coordinate relativization. |
| **Security & Bounds** | 9.5 | Strict workspace path confinement; recursion stack-overflow immunity; bounded import traversal. |
| **Invariants** | 9.3 | Pure functional parsers `(file, content) -> Result<Vec<Symbol>>`; deterministic coordinate normalization. |

**Overall Health Score**: **9.3 / 10.0** (Status: **MINOR**)

---

## 2. Verification of Core Remediations

### 1. Go Cross-Package Calls & Selector Disambiguation
- **Verification Target**: [`src/parser/go.rs#L26-L64`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L26-L64), [`#L253-L262`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L253-L262)
- **Mechanics**: `collect_file_imported_packages` extracts all imported package names and aliases. In `collect_references`, `selector_expression` checks whether `operand` matches an imported package. If true, `is_member` is cleared to `false`, preventing spurious confinement to file-local resolution. True struct receiver methods retain `is_member = true`.
- **Proof**: Verified by integration test `test_cli_go_cross_package_call_disambiguation`, where `service.ProcessData()` resolves as a callee edge from `Run`.

### 2. Python Absolute Imports & PYTHON_STDLIB Whitelist
- **Verification Target**: [`src/parser/python.rs#L248-L335`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L248-L335)
- **Mechanics**: Absolute imports (`import ...`, `from ... import ...`) are checked against a curated `PYTHON_STDLIB` list of 37 standard modules (`os`, `sys`, `json`, `pathlib`, `typing`, etc.). Only standard library symbols are placed into `external_imports`. Project-internal absolute imports (`from models import User`) remain resolvable within the workspace graph.
- **Proof**: Verified by integration test `test_cli_python_absolute_import_not_external`, confirming `models.User` creates an active callee edge.

### 3. Slice Line Numbering Formatting & Pipe Preservation
- **Verification Target**: [`src/model/slice.rs#L271-L285`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/slice.rs#L271-L285)
- **Mechanics**: `format_body` checks `self.symbol.is_none()` before attempting to split line-prefix markers (`{:4} | `). Symbol slices contain raw source bodies and are formatted directly via `(start_line + i, line)`, never invoking `split_once(" | ")`. Line-range slices split on the first pipe, preserving subsequent code pipes intact.
- **Proof**: Verified by unit test `test_numbered_render_preserves_pipe_characters_in_symbol_slice`, confirming pattern matching (`1 | 2 => true`) retains line structure and prefix syntax without corruption.

### 4. AST Walker & Import Collector Depth Enforcement (MAX_AST_DEPTH = 512)
- **Verification Target**: [`src/parser/rust.rs#L25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs#L25), [`src/parser/typescript.rs#L37`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L37), [`src/parser/python.rs#L25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L25), [`src/parser/go.rs#L25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L25)
- **Mechanics**: `MAX_AST_DEPTH` constant increased to `512`. All primary AST walkers (`walk_*_node`), reference collectors (`collect_references`), and import collectors (`collect_uses`, `collect_import_binding`, `collect_py_imports`, `collect_go_imports`, `collect_go_all_imports`) enforce `depth >= MAX_AST_DEPTH` guards to prevent stack overflow on deeply nested ASTs.

---

## 3. Invariant Compliance Audit

| Invariant | Status | Evidence |
|---|:---:|---|
| **Content-Hash Driven** | **PASS** | Parsers operate purely on in-memory buffers; hash verification owned upstream in storage sync. |
| **Deterministic & Non-Interactive** | **PASS** | Zero user prompting; deterministic symbol vectors; strict error bails on malformed coordinates. |
| **Workspace Confinement** | **PASS** | `Coordinate::normalize_against` strips root prefixes; relative coordinates never escape repository seam. |
| **Zero Background Daemons** | **PASS** | All parsers execute synchronously on demand within ephemeral CLI/MCP call lifecycle. |
| **Purity Split** | **PASS** | Pure functional core `(file, content) -> Result<Vec<Symbol>>`; zero side effects or I/O in parser layer. |

---

## 4. Residual Observations & Minor Tradeoffs

1. **Unpopulated Mentions in Python & Go** (P2): [`src/parser/python.rs#L139`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L139) and [`src/parser/go.rs#L206`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L206) hardcode `mentions = Vec::new()`. Type mentions, composite literal constructors (`Server{}`), and parameter references are not surfaced into `Symbol.mentions` (unlike Rust and TypeScript).
2. **Per-File Parser Allocation** (P3): Each `parse_*` call instantiates `Parser::new()` and sets language. A thread-local pool would reduce allocator churn on large codebases.
3. **Seam Typing in Slice Content** (P3): [`src/model/slice.rs#L14`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/slice.rs#L14) stores formatted text for line ranges in `content` rather than raw lines, necessitating presentation-level parsing.
4. **Python Receiver Attribute Calls** (P3): Calls using receiver syntax (`utils.calc()`) produce `attribute` nodes, setting `is_member = true` and constraining to same-file resolution unless imported via `from utils import calc`.

---

## 5. Conclusion
Scope 1 has completed all required remediations with zero critical findings and zero invariant breaches. All regression suites, polyglot tests, and lint checks pass cleanly.
