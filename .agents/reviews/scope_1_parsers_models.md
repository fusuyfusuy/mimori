---
scope: "Parsers & Models"
score: 6.8
status: "CRITICAL"
critical_findings: 5
invariant_breaches: []
---

# Scope 1 Audit: Parsers & AST Models

## Executive Summary
Audit of the parser and AST representation boundary across Rust, TypeScript, Python, and Go reveals severe correctness defects in Go grouped type extraction, Python typed signatures, and Windows coordinate parsing, alongside unbounded recursion risks and massive heap churn from redundant string allocations in call-edge collectors.

## 1. Correctness
- **Go Grouped Type AST Mismatch** ([`src/parser/go.rs:62`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L62)): In `type ( A struct{}; B interface{} )`, `walk_go_node` passes the outer `type_declaration` node to `create_symbol` instead of the child `type_spec`. All grouped types inherit the coordinate lines, signature, and body of the entire block.
- **Python Signature Truncation on Type Annotations** ([`src/parser/python.rs:170`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L170)): `extract_signature` matches `body.find(':')`. Any typed parameter (`def foo(x: int)`) causes truncation at the first parameter colon (`def foo(x`), discarding parameters and return types.
- **Python Decorator Stripping** ([`src/parser/python.rs:68-73`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L68-L73)): In `decorated_definition`, traversal delegates to `def_node`. `create_symbol` uses `def_node.start_position()`, stripping all decorators (`@app.get`, `@dataclass`) from symbol body and line coordinates.
- **TypeScript Parameter Object Signature Truncation** ([`src/parser/typescript.rs:434-438`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L434-L438)): `extract_signature` slices at `body.find('{')`. Arrow functions with destructured parameters (`const f = ({ id }: Props) => ...`) truncate at the opening brace (`const f = (`).
- **Windows Drive-Letter Coordinate Failure** ([`src/model/coordinate.rs:51-62`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/coordinate.rs#L51-L62)): `Coordinate::parse` uses `raw.find(':')`. On Windows, `C:\repo\file.rs:symbol` splits at index 1 (`head = "C"`). `looks_like_path("C")` evaluates to false, misclassifying valid file symbols as `Coordinate::Bare`.
- **Missing Mentions in Python & Go** ([`src/parser/python.rs:109`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L109), [`src/parser/go.rs:119`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L119)): `mentions` is left empty in Python and Go, losing identifier and property references used for `uses` queries.
- **Fragile Ad-Hoc Import String Parsing** ([`src/parser/rust.rs:260-345`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs#L260-L345), [`src/parser/python.rs:187-228`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L187-L228)): Parsers bypass tree-sitter AST queries in favor of string slicing (`strip_prefix("pub")`, `split_once(" import ")`), failing on attributes (`#[cfg(test)]`) and comments.

## 2. Robustness
- **Bitwise OR & Pattern Line Number Corruption** ([`src/model/slice.rs:275-285`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/slice.rs#L275-L285)): In `format_body(numbered=true)`, `line.split_once(" | ")` tests if the LHS parses as `usize`. Any line beginning with an integer followed by ` | ` (e.g. `1 | 2 => ...` or `0 | flags`) clobbers the rendered line number and truncates code text.
- **Unvalidated Coordinate Line Ranges** ([`src/model/coordinate.rs:121-132`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/coordinate.rs#L121-L132)): `parse_line_range` does not assert `start <= end` or `start >= 1`.
- **Inaccurate Slicing Token Estimation** ([`src/model/slice.rs:36`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/slice.rs#L36)): Uses raw byte length `s.len()` instead of character/grapheme count, heavily penalizing multi-byte UTF-8 sources.

## 3. Performance
- **Gratuitous String Allocation in Membership Checks** ([`src/parser/rust.rs:180`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs#L180), [`src/parser/typescript.rs:327`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L327), [`src/parser/python.rs:155`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L155), [`src/parser/go.rs:164`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L164)): `push_call`, `push_mention`, and `push_import` execute `!calls.contains(&name.to_string())`, heap-allocating a new `String` on every visited AST reference even when already present.
- **Redundant Clones of File-Level External Imports** ([`src/parser/rust.rs:19`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs#L19), [`src/parser/typescript.rs:31`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L31)): `external_imports` is cloned into every `Symbol` struct in the file, causing $O(S \times I)$ heap allocation churn.
- **Eager Slice Body Duplication** ([`src/model/symbol.rs:46`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/symbol.rs#L46)): `Symbol` eagerly clones entire method and function bodies into owned `String`s rather than retaining byte range offsets.

## 4. Security & DoS
- **Unbounded AST Call Stack Recursion** ([`src/parser/rust.rs:25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/rust.rs#L25), [`src/parser/typescript.rs:37`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L37), [`src/parser/python.rs:25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/python.rs#L25), [`src/parser/go.rs:25`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/go.rs#L25)): AST traversal and reference collection use naive call stack recursion. Deeply nested ASTs (e.g. 5,000 chained calls, nested JSX elements, or generated AST structures) trigger native thread stack overflow crashes (SIGSEGV).

## Remediation Roadmap
1. Pass `child` (`type_spec`) in `src/parser/go.rs:62`.
2. Extract signatures via Tree-sitter AST nodes (parameters, return types) rather than naive `find(':')` / `find('{')`.
3. Wrap `decorated_definition` in Python to include decorator nodes in the symbol span.
4. Support Windows drive letters in `Coordinate::parse` by checking for `^[A-Za-z]:[\\/]` before splitting on `:`.
5. Replace `calls.contains(&name.to_string())` with `calls.iter().any(|c| c == name)`.
6. Convert recursive AST walking to an iterative worklist with a maximum depth guard (e.g. 256).
