# Mimori Empirical Field Report & Roadmap Recommendations

**Date**: 2026-10-31  
**Source Session**: `fusuycorp/hepyeni` (`/boundary-review`, `/boundary-refine`, `fix all`, and `argon` adversarial diff audit across 46 modified files)  
**Target Binary**: `mimori` v2.4.9 (`/home/devhax/projects/fusuyfusuy/mimori`)

---

## Part 1: Core Engine & CLI Improvements (`mimori` Rust Crate)

### 1. [P1] Resolve Workspace-Local Import Aliases (`import { X as Y }`)
- **Empirical Repro (`hepyeni`)**:
  In `src/lib/importers/types.ts:6`, `isSafeCoverUrl` was imported under a local alias:
  ```ts
  import { isSafeCoverUrl as isSafeHttpUrl } from "@/lib/providers/validation";
  ```
  Running `mimori blast isSafeCoverUrl` returned 22 upstream symbols (`normalizeProviderResult`, `saveMediaProgress`, `addTitle`, `addCustomTitle`), **completely omitting `normalizeImportRow` in `src/lib/importers/types.ts`**. Running `mimori blast isSafeHttpUrl` returned `Error: Target symbol 'isSafeHttpUrl' not found in workspace`.
- **Root Cause in `mimori`**:
  [`src/parser/typescript.rs:collect_import_binding`](file:///home/devhax/projects/fusuyfusuy/mimori/src/parser/typescript.rs#L516-L540) (`L524-L532`) inspects `import_specifier` nodes (`alias` vs `name`) only for `collect_external_imports` (to block external npm imports from colliding with local symbol names). For internal/alias-resolved workspace imports (`@/lib/...` or `./...`), calls to `isSafeHttpUrl(...)` inside `normalizeImportRow` emit a raw callee reference `"isSafeHttpUrl"`, which fails to match the exported symbol `"isSafeCoverUrl"` in [`src/graph/mod.rs:SymbolGraph::new`](file:///home/devhax/projects/fusuyfusuy/mimori/src/graph/mod.rs#L78-L356).
- **Fix Specification**:
  1. During AST parsing in `src/parser/typescript.rs` (and `use a::b as c` in `src/parser/rust.rs`, `from a import b as c` in `src/parser/python.rs`), collect a per-file `HashMap<String, String>` mapping `local_alias -> original_exported_name` for non-external imports.
  2. When extracting `calls` and `uses` within that file, rewrite any callee identifier matching `local_alias` to `original_exported_name` (or `resolved_module_path:original_exported_name`) before inserting into `Symbol.calls` / `Symbol.uses`.

---

### 2. [P1] New Subcommand: `mimori diff` (or `mimori blast --dirty`)
- **Empirical Gap**:
  During multi-file refactors (`fix all` across 46 files) or adversarial diff reviews (`/diff-inquisitor` / `argon`), agents must run `git diff`, manually map hunk line numbers back to enclosing function names, and invoke `mimori blast <Symbol>` N times sequentially. Because of that multi-step friction, agents skip `mimori blast` on diffs.
- **Fix Specification**:
  Add `mimori diff [--staged] [--base <rev>] [-d|--depth <N>] [--json]`:
  1. Shell out to `git diff -U0 <base>` and parse `@@ -a,b +c,d @@` hunk headers into `(file_path, start_line..=end_line)`.
  2. Intersect changed line spans with indexed `Symbol` spans (`sym.start_line..=sym.end_line`) in [`src/graph/mod.rs:SymbolGraph`](file:///home/devhax/projects/fusuyfusuy/mimori/src/graph/mod.rs#L58-L75).
  3. Emit a single unified M2M report containing:
     - **Mutated Symbols** (`file:symbol (Lstart-Lend)`)
     - **Combined Upstream Blast Cone** (`d=1..N` callers outside the diff)
     - **Affected Test Suites** (union of all transitive test callers)
     - **Debt & Memory Gate** (`mimori debt check` + `mimori memory lint` status on touched files)

---

### 3. [P1] Tolerant Coordinate Parsing (`file.ts#Symbol` and `file.ts#L10-L20`)
- **Empirical Repro (`hepyeni`)**:
  Agents trained on Markdown/GitHub links (`path/to/file.ts#Symbol` or `path/to/file.ts#L10-L20`) frequently pass `#` instead of `:` to `mimori slice`:
  ```bash
  mimori slice "src/components/group-schedules-card.tsx#GroupSchedulesCard"
  # Error: Symbol 'src/components/group-schedules-card.tsx#GroupSchedulesCard' not found in workspace.
  ```
  After one CLI error, agents typically abandon `mimori` and fall back to `read_file`.
- **Root Cause in `mimori`**:
  [`src/model/coordinate.rs:Coordinate::parse`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/coordinate.rs#L30-L78) (`L36-L73`) only recognizes `:#L` for line ranges and `:` for `file:symbol` splits. A coordinate like `src/foo.ts#Bar` falls through to `Coordinate::Bare { name: "src/foo.ts#Bar" }` (`L75`).
- **Fix Specification**:
  In [`Coordinate::parse`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/coordinate.rs#L30-L78) and [`parse_line_range`](file:///home/devhax/projects/fusuyfusuy/mimori/src/model/coordinate.rs#L132-L143):
  1. Accept both `:#L10-20` and `#L10-L20` / `:L10-L20` (stripping optional leading `L` on the end line in `parse_line_range`).
  2. When no `:` separator is present (or after drive-letter handling), if `raw.find('#')` splits `(head, tail)` where `looks_like_path(head)` is true and `!tail.is_empty()`, parse it as `Coordinate::Symbol { file: PathBuf::from(head), name: tail.to_string() }`.

---

### 4. [P2] File-Scoped "Did You Mean?" Candidates When `file:Symbol` Fails
- **Empirical Repro (`hepyeni`)**:
  Running `mimori slice "src/lib/actions/llm-extract.ts:extractTitlesFromText"` (when the real function was `extractTitlesFromDump`) output:
  ```text
  Error: Symbol 'src/lib/actions/llm-extract.ts:extractTitlesFromText' not found in workspace.
  Try: mimori find 'src/lib/actions/llm-extract.ts:extractTitlesFromText'
  ```
  Running that suggested `mimori find` command with the `file:Symbol` string also returns `0 matches`.
- **Root Cause in `mimori`**:
  [`src/graph/mod.rs:SymbolGraph::resolve`](file:///home/devhax/projects/fusuyfusuy/mimori/src/graph/mod.rs#L486-L534) returns `Resolution::NotFound` without checking whether the `file` component of `Coordinate::Symbol { file, name }` matched indexed symbols in that file.
- **Fix Specification**:
  When `Coordinate::Symbol { file, name }` resolves to 0 symbols for `name`, check if `file` matches any indexed file in `SymbolGraph`. If so, list the symbols defined in that file (ranked by prefix/edit distance to `name`) directly in the error message:
  ```text
  Error: Symbol 'extractTitlesFromText' not found in 'src/lib/actions/llm-extract.ts'.
  Symbols defined in src/lib/actions/llm-extract.ts:
    - src/lib/actions/llm-extract.ts:extractTitlesFromDump (L45-L147)
    - src/lib/actions/llm-extract.ts:proposeExtractedTitles (L149-L346)
  ```

---

### 5. [P2] Call-Site Line Numbers on `d=1` Edges in `mimori blast` & `mimori up`
- **Empirical Repro (`hepyeni`)**:
  `mimori blast handleToggleCheckin` reported:
  ```text
  - (d=1) GroupSchedulesCard (function) → src/components/group-schedules-card.tsx:GroupSchedulesCard
  ```
  Because `GroupSchedulesCard` spans `L65-L643` (578 lines) and invokes `handleToggleCheckin` at two distinct lines (`L478` and `L603`), the agent still had to grep or read large chunks of the file to locate the call sites.
- **Fix Specification**:
  Record call-site 1-based line numbers alongside callee names during AST extraction, and display `[calls @ L478, L603]` on `d=1` edges in [`src/graph/blast.rs:BlastResult::to_markdown`](file:///home/devhax/projects/fusuyfusuy/mimori/src/graph/blast.rs#L53-L131) and `mimori up`.

---

### 6. [P2] Automatic Test-File Literal Sweep in `mimori blast`
- **Empirical Repro (`hepyeni`)**:
  `tests/argon-audit-ws4.test.ts:70` reads `src/components/group-schedules-card.tsx` as raw text (`Bun.file().text()`) and matches `/await\s+handleToggleCheckin\(milestoneId\)/`. Because there is no AST import/call edge, `mimori blast handleToggleCheckin` listed **0** `Affected Test Suites`, and changing `handleToggleCheckin`'s signature broke `bun test`.
- **Fix Specification**:
  In [`src/graph/blast.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/graph/blast.rs), augment AST test reachability (`is_test_symbol`, `L330-L368`) with a fast Rayon literal scan restricted to test files (`tests/**`, `**/*.test.*`, `**/*.spec.*`) for the target symbol name. Emit non-AST matches under a separate heading:
  ```text
  ### 🔍 Literal / Source-Inspection Test Mentions
  - 🔍 `tests/argon-audit-ws4.test.ts:70`
  ```

---

### 7. [P3] CLI Flag Consistency: Add `--scope <dir_or_file>` to `mimori find`
- **Empirical Repro**:
  `mimori map`, `mimori missing`, and `mimori debt` all accept `--scope <dir>`, whereas `mimori find --scope src/lib/actions <pattern>` exits with `error: unexpected argument '--scope' found`.
- **Fix Specification**:
  Add `#[arg(long)] pub scope: Option<String>` to `FindArgs` in [`src/cli/args.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/cli/args.rs) (and `mimori_find` in [`src/mcp/tools.rs`](file:///home/devhax/projects/fusuyfusuy/mimori/src/mcp/tools.rs)) to filter file and symbol matches by path prefix.

---

## Part 2: Agent Harness & Prompting Recommendations (Increasing Agent Adoption)

1. **Promote `mimori` MCP Tools from `Lazy` to `Eager`**:
   - When `mimori_*` MCP tools are registered as `Lazy`, calling them costs 2 turns (schema file read + `call_mcp_tool`), whereas native `read_file` costs 1 turn. Marking `mimori_dump`, `mimori_slice`, `mimori_blast`, `mimori_find`, and `mimori_debt` as **Eager** removes the tool-call tax.
2. **Enforce Sequencing Gates in Global `AGENTS.md` (Section 4 & Section 7)**:
   - Replace passive descriptions with blocking invariants:
     - **Turn-0 Gate**: First tool call in any Sovereign workspace (main agent or subagent) MUST be `mimori dump --budget 1500`.
     - **Pre-Mutation Blast Gate**: Before calling `edit_file` on any function/type signature, MUST execute `mimori blast <Symbol>` and record `Affected: N`.
     - **Subagent Inheritance**: Custom subagents (`define_subagent`) in Sovereign workspaces MUST set `EnableMCPTools: true` and include the `mimori` coordinate syntax (`file:Symbol`, never `file#Symbol`) in their prompt.
3. **Require Literal `mimori blast` Headers in Plan/Refinement Artifacts**:
   - In `/boundary-refine` and `/plan` workflows, require each task to paste the literal `mimori blast <Symbol>` header (`Depth: 3, Affected: N`). Agents cannot guess exact transitive caller counts without actually invoking `mimori blast`.
