---
scope: "Parsers & AST Ingestion"
score: 8.0
status: "MODERATE"
critical_findings: 0
invariant_breaches:
  - "Go and Python parsers initialize empty mentions and never populate them, breaking cross-language parity for `mimori uses`."
---

# Scope 1: Parsers & AST Ingestion Audit

## Overview
Evaluated polyglot Tree-sitter parsers: [`src/parser/mod.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/mod.rs), [`src/parser/lang.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/lang.rs), [`src/parser/rust.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/rust.rs), [`src/parser/typescript.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/typescript.rs), [`src/parser/python.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/python.rs), and [`src/parser/go.rs`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/go.rs).

## Findings

### 1. Parity Defect: Missing Mention Collection in Go and Python (Moderate)
- **Location**: [`src/parser/python.rs#L139`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/python.rs#L139), [`src/parser/go.rs#L209`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/go.rs#L209)
- **Detail**: In `python.rs` and `go.rs`, `create_symbol()` creates an empty `let mentions = Vec::new();` and never populates it during AST traversal. In contrast, `typescript.rs#L234-L382` collects parameter mentions, property reads, and template interpolations, and `rust.rs#L226-L231` collects `type_identifier` nodes. Consequently, `mimori uses <symbol>` fails silently for non-call mentions in Python and Go codebases.

### 2. Parity Defect: Rust Type Mentions vs Value Mentions (Minor)
- **Location**: [`src/parser/rust.rs#L226-L231`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/rust.rs#L226-L231)
- **Detail**: Rust AST traversal only records `type_identifier` nodes into `mentions`. Struct field accesses (`s.field`), enum variants without call parens (`Option::None`), and variable arguments passed to macros/functions are not recorded as mentions, reducing graph recall compared to TypeScript.

### 3. Tree-sitter Traversal Depth Limit (Minor / Robustness)
- **Location**: [`src/parser/rust.rs#L25`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/rust.rs#L25), [`src/parser/typescript.rs#L37`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/typescript.rs#L37), [`src/parser/python.rs#L25`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/python.rs#L25), [`src/parser/go.rs#L22`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/go.rs#L22)
- **Detail**: All parsers enforce `const MAX_AST_DEPTH: usize = 512;`. This prevents recursion stack overflow on adversarial or generated ASTs. Clean invariant protection.

### 4. Lossy Path Conversion in `parse_file` (Minor)
- **Location**: [`src/parser/mod.rs#L16`](file:///home/fusuyfusuy/Projects/fusuyfusuy/mimori/src/parser/mod.rs#L16)
- **Detail**: `path.to_str().unwrap_or("")` drops the file path if it contains non-UTF-8 bytes, leading to symbols recorded with an empty `file` attribute. Using `path.to_string_lossy()` is safer.

## Recommendations
1. Implement `collect_references` mentions extraction in `python.rs` (attribute nodes, type annotations, arguments) and `go.rs` (selector expressions, type names, parameter identifiers).
2. Align `rust.rs` mentions collection with `field_expression` identifier reads.
3. Replace `path.to_str().unwrap_or("")` with `path.to_string_lossy()`.
