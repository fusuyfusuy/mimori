use crate::graph::blast::{traverse_multi_upstream, BlastNode};
use crate::graph::SymbolGraph;
use crate::memory::{check_debt, MemoryLedger};
use crate::workspace::walker::MAX_INDEX_FILE_BYTES;
use crate::workspace::AliasSet;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_GIT_DIFF_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GIT_STDERR_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutatedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub coordinate: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffBlastReport {
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    pub mutated_symbols: Vec<MutatedSymbol>,
    pub affected: Vec<BlastNode>,
    pub entry_points: Vec<String>,
    pub test_suites: Vec<String>,
    pub literal_test_mentions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_member_calls: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_notes: Vec<String>,
    pub debt_status: String,
    pub memory_status: String,
}

impl DiffBlastReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Diff Blast Report (Touched Files: {}, Mutated Symbols: {}, Upstream Affected: {})\n\n",
            self.touched_files.len(),
            self.mutated_symbols.len(),
            self.affected.len()
        ));

        if let Some(warn) = &self.unverified_member_calls {
            out.push_str(&format!("> {}\n\n", warn));
        }

        if !self.memory_notes.is_empty() {
            out.push_str("### Scoped Memory\n");
            for note in &self.memory_notes {
                out.push_str(&format!("- {}\n", note));
            }
            out.push('\n');
        }

        if self.touched_files.is_empty() {
            out.push_str("Working tree diff is clean (0 changed hunks).\n\n");
        } else {
            if !self.deleted_files.is_empty() {
                out.push_str("### Deleted Files\n");
                for f in &self.deleted_files {
                    out.push_str(&format!("- `{}`\n", f));
                }
                out.push('\n');
            }
            out.push_str("### Mutated Symbols\n");
            if self.mutated_symbols.is_empty() {
                out.push_str("No indexed symbols overlapped the diff hunks.\n");
            } else {
                for m in &self.mutated_symbols {
                    out.push_str(&format!(
                        "- **`{}`** ({}) -> `{}` (L{}-L{})\n",
                        m.name, m.kind, m.coordinate, m.start_line, m.end_line
                    ));
                }
            }
            out.push('\n');
        }

        if !self.entry_points.is_empty() {
            out.push_str("### Affected Entry Points\n");
            for ep in &self.entry_points {
                out.push_str(&format!("- `{}`\n", ep));
            }
            out.push('\n');
        }

        if !self.test_suites.is_empty() {
            out.push_str("### Affected Test Suites\n");
            for ts in &self.test_suites {
                out.push_str(&format!("- `{}`\n", ts));
            }
            out.push('\n');
        }

        if !self.affected.is_empty() {
            out.push_str("### Combined Upstream Blast Cone (External to Diff)\n");
            for node in &self.affected {
                let indent = "  ".repeat(node.depth);
                let tag = if node.is_entry_point {
                    " [Entry Point]"
                } else if node.is_test {
                    " [Test]"
                } else {
                    ""
                };
                let call_at = if node.call_lines.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> =
                        node.call_lines.iter().map(|l| format!("L{l}")).collect();
                    format!(" [calls @ {}]", formatted.join(", "))
                };
                out.push_str(&format!(
                    "{}- (d={}) **`{}`** ({}) -> `{}`{}{}\n",
                    indent, node.depth, node.name, node.kind, node.coordinate, call_at, tag
                ));
            }
            out.push('\n');
        }

        if !self.literal_test_mentions.is_empty() {
            out.push_str("### Literal / Source-Inspection Test Mentions\n");
            for m in &self.literal_test_mentions {
                out.push_str(&format!("- `{}`\n", m));
            }
            out.push('\n');
        }

        out.push_str("### Debt & Memory Gate\n");
        out.push_str(&format!("- **Debt**: {}\n", self.debt_status.trim()));
        out.push_str(&format!("- **Memory**: {}\n", self.memory_status.trim()));
        out
    }
}

pub fn validate_diff_base(base: Option<&str>) -> Result<()> {
    if let Some(b) = base {
        if b.trim().is_empty() || b.starts_with('-') || b.chars().any(|c| c.is_control()) {
            bail!(
                "Invalid diff base revision '{}': must not be empty, start with '-', or contain control characters",
                b
            );
        }
    }
    Ok(())
}

fn run_git_command_bounded(
    root: &Path,
    args: &[&str],
    max_stdout: u64,
    label: &str,
) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to execute '{label}'"))?;
    let stdout_pipe = child.stdout.take().context("missing git stdout pipe")?;
    let stderr_pipe = child.stderr.take().context("missing git stderr pipe")?;

    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.take(max_stdout + 1).read_to_end(&mut buf);
        let truncated = buf.len() as u64 > max_stdout;
        if truncated {
            buf.truncate(max_stdout as usize);
        }
        (buf, truncated)
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.take(MAX_GIT_STDERR_BYTES).read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + GIT_DIFF_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("{label} timed out after {}s", GIT_DIFF_TIMEOUT.as_secs());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    let (stdout_bytes, truncated) = out_handle.join().unwrap_or_default();
    let stderr_bytes = err_handle.join().unwrap_or_default();

    if truncated {
        bail!("{label} output exceeded {} byte safety cap", max_stdout);
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        bail!("{label} exited with error: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&stdout_bytes).into_owned())
}

fn run_git_diff_with_timeout(root: &Path, staged: bool, base: Option<&str>) -> Result<String> {
    let mut args = vec![
        "-c",
        "core.quotepath=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "-c",
        "diff.noprefix=false",
        "diff",
        "--relative",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "-U0",
    ];
    if staged {
        args.push("--staged");
    }
    if let Some(b) = base {
        args.push(b);
    }
    args.push("--");
    run_git_command_bounded(root, &args, MAX_GIT_DIFF_BYTES, "git diff")
}

fn run_git_show_index_blob(root: &Path, rel_path: &str) -> Option<String> {
    let clean = rel_path.trim_start_matches("./");
    if clean.is_empty() || clean.starts_with('/') || clean.contains('\0') {
        return None;
    }
    let spec = format!(":./{clean}");
    run_git_command_bounded(root, &["show", &spec], MAX_INDEX_FILE_BYTES, "git show").ok()
}

fn hunk_overlaps_span(h: &DiffHunk, start_line: usize, end_line: usize) -> bool {
    if h.is_deletion {
        h.start > 0 && start_line <= h.start && h.start <= end_line
    } else {
        start_line <= h.end && end_line >= h.start
    }
}

fn match_staged_symbol_to_graph(
    graph: &SymbolGraph,
    file_syms: &[usize],
    staged_sym: &crate::model::Symbol,
) -> Option<usize> {
    let exact_kind = file_syms
        .iter()
        .copied()
        .filter(|&i| {
            let g = &graph.symbols[i];
            g.name == staged_sym.name && g.kind == staged_sym.kind
        })
        .min_by_key(|&i| graph.symbols[i].start_line.abs_diff(staged_sym.start_line));
    if exact_kind.is_some() {
        return exact_kind;
    }
    file_syms
        .iter()
        .copied()
        .filter(|&i| graph.symbols[i].name == staged_sym.name)
        .min_by_key(|&i| graph.symbols[i].start_line.abs_diff(staged_sym.start_line))
}

pub fn calculate_diff_blast(
    root: &Path,
    graph: &SymbolGraph,
    staged: bool,
    base: Option<&str>,
    depth: usize,
) -> Result<DiffBlastReport> {
    validate_diff_base(base)?;

    let diff_text = run_git_diff_with_timeout(root, staged, base)?;
    let parsed = parse_unified_diff_full(&diff_text);

    let mut touched_files = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();
    for f in parsed
        .hunks
        .iter()
        .map(|h| h.file.as_str())
        .chain(parsed.extra_touched.iter().map(String::as_str))
        .chain(parsed.deleted_files.iter().map(String::as_str))
    {
        if seen_files.insert(f.to_string()) {
            touched_files.push(f.to_string());
        }
    }

    let mut syms_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, sym) in graph.symbols.iter().enumerate() {
        if seen_files.contains(sym.file.as_str()) {
            syms_by_file.entry(sym.file.as_str()).or_default().push(idx);
        }
    }

    let mut hunks_by_file: HashMap<&str, Vec<&DiffHunk>> = HashMap::new();
    for h in &parsed.hunks {
        hunks_by_file.entry(h.file.as_str()).or_default().push(h);
    }

    let mut mutated_indices = Vec::new();
    let mut seen_mutated = HashSet::new();
    let empty_aliases = AliasSet::default();

    for (file, file_hunks) in &hunks_by_file {
        let Some(file_syms) = syms_by_file.get(file) else {
            continue;
        };

        let staged_parsed = if staged {
            run_git_show_index_blob(root, file).and_then(|blob| {
                crate::parser::parse_file(Path::new(file), &blob, &empty_aliases).ok()
            })
        } else {
            None
        };

        if let Some(staged_syms) = staged_parsed {
            for staged_sym in &staged_syms {
                if file_hunks
                    .iter()
                    .any(|h| hunk_overlaps_span(h, staged_sym.start_line, staged_sym.end_line))
                {
                    if let Some(idx) = match_staged_symbol_to_graph(graph, file_syms, staged_sym) {
                        if seen_mutated.insert(idx) {
                            mutated_indices.push(idx);
                        }
                    }
                }
            }
        } else {
            for h in file_hunks {
                for &idx in file_syms {
                    let sym = &graph.symbols[idx];
                    if hunk_overlaps_span(h, sym.start_line, sym.end_line)
                        && seen_mutated.insert(idx)
                    {
                        mutated_indices.push(idx);
                    }
                }
            }
        }
    }
    mutated_indices.sort_unstable();

    let mutated_symbols: Vec<MutatedSymbol> = mutated_indices
        .iter()
        .map(|&i| {
            let s = &graph.symbols[i];
            MutatedSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                file: s.file.clone(),
                coordinate: s.coordinate(),
                start_line: s.start_line,
                end_line: s.end_line,
            }
        })
        .collect();

    let upstream = traverse_multi_upstream(graph, &mutated_indices, depth);

    let file_refs: Vec<&str> = touched_files.iter().map(String::as_str).collect();
    let sym_refs: Vec<&str> = mutated_symbols.iter().map(|s| s.name.as_str()).collect();
    let memory_notes = MemoryLedger::scoped_notes_for_workspace(root, &file_refs, &sym_refs);

    let (_debt_ok, debt_status) = check_debt(root);
    let memory_status = match crate::memory::ledger::confine_agents_path(root, ".agents/memory.md")
    {
        Ok(p) if p.exists() => match MemoryLedger::load(root) {
            Ok(ledger) => ledger.lint().to_m2m_output(),
            Err(e) => format!("MEM_ERR: {}", e),
        },
        Ok(_) => "MEM_SKIP: no .agents/memory.md present.".to_string(),
        Err(e) => format!("MEM_ERR: {}", e),
    };

    Ok(DiffBlastReport {
        touched_files,
        deleted_files: parsed.deleted_files,
        mutated_symbols,
        affected: upstream.affected,
        entry_points: upstream.entry_points,
        test_suites: upstream.test_suites,
        literal_test_mentions: upstream.literal_test_mentions,
        unverified_member_calls: upstream.unverified_member_calls,
        memory_notes,
        debt_status,
        memory_status,
    })
}

struct DiffHunk {
    file: String,
    start: usize,
    end: usize,
    is_deletion: bool,
}

struct ParsedDiff {
    hunks: Vec<DiffHunk>,
    deleted_files: Vec<String>,
    extra_touched: Vec<String>,
}

fn unquote_diff_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed.split('\t').next().unwrap_or(trimmed).trim();
    if stripped.len() < 2 || !stripped.starts_with('"') || !stripped.ends_with('"') {
        return stripped.to_string();
    }
    let inner = &stripped[1..stripped.len() - 1];
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'0'..=b'7'
                    if i + 3 < bytes.len()
                        && bytes[i + 1..i + 4]
                            .iter()
                            .all(|b| (b'0'..=b'7').contains(b)) =>
                {
                    let val = ((bytes[i + 1] - b'0') as u16 * 64)
                        + ((bytes[i + 2] - b'0') as u16 * 8)
                        + ((bytes[i + 3] - b'0') as u16);
                    if val <= 255 {
                        out.push(val as u8);
                        i += 4;
                        continue;
                    }
                    out.push(bytes[i]);
                    i += 1;
                }
                other => {
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_ab_prefix(path: String, prefix: &str) -> String {
    if let Some(rest) = path.strip_prefix(prefix) {
        rest.to_string()
    } else {
        path
    }
}

fn parse_unified_diff_full(diff_text: &str) -> ParsedDiff {
    let mut hunks = Vec::new();
    let mut deleted_files = Vec::new();
    let mut extra_touched = Vec::new();
    let mut old_file: Option<String> = None;
    let mut current_file: Option<String> = None;
    let mut old_rem = 0usize;
    let mut new_rem = 0usize;

    for line in diff_text.lines() {
        if old_rem > 0 || new_rem > 0 {
            if line.starts_with('\\') {
                continue;
            } else if line.starts_with('+') {
                new_rem = new_rem.saturating_sub(1);
                continue;
            } else if line.starts_with('-') {
                old_rem = old_rem.saturating_sub(1);
                continue;
            } else if line.starts_with(' ') || line.is_empty() {
                old_rem = old_rem.saturating_sub(1);
                new_rem = new_rem.saturating_sub(1);
                continue;
            } else {
                // Unexpected header while counts non-zero: resync cleanly.
                old_rem = 0;
                new_rem = 0;
            }
        }

        if line.starts_with("diff --git ") {
            old_file = None;
            current_file = None;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            old_file = Some(unquote_diff_path(rest));
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            let dest = unquote_diff_path(rest);
            extra_touched.push(dest.clone());
            current_file = Some(dest);
        } else if let Some(rest) = line.strip_prefix("--- ") {
            let p = unquote_diff_path(rest);
            old_file = if p == "/dev/null" {
                None
            } else {
                Some(strip_ab_prefix(p, "a/"))
            };
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let p = unquote_diff_path(rest);
            if p == "/dev/null" {
                if let Some(ref deleted) = old_file {
                    deleted_files.push(deleted.clone());
                }
                current_file = None;
            } else {
                current_file = Some(strip_ab_prefix(p, "b/"));
            }
        } else if line.starts_with("@@ ") {
            if let Some((old_count, start, end, new_count, is_deletion)) =
                parse_hunk_header_counts(line)
            {
                old_rem = old_count;
                new_rem = new_count;
                if let Some(file) = &current_file {
                    hunks.push(DiffHunk {
                        file: file.clone(),
                        start,
                        end,
                        is_deletion,
                    });
                }
            }
        }
    }

    ParsedDiff {
        hunks,
        deleted_files,
        extra_touched,
    }
}

pub fn parse_unified_diff(diff_text: &str) -> Vec<(String, usize, usize)> {
    parse_unified_diff_full(diff_text)
        .hunks
        .into_iter()
        .map(|h| (h.file, h.start.max(1), h.end.max(1)))
        .collect()
}

fn parse_range_count(token: &str) -> Option<(usize, usize)> {
    let (start_str, count_str) = match token.split_once(',') {
        Some((s, c)) => (s, Some(c)),
        None => (token, None),
    };
    let start: usize = start_str.parse().ok()?;
    let count: usize = match count_str {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

fn parse_hunk_header_counts(line: &str) -> Option<(usize, usize, usize, usize, bool)> {
    let body = line.strip_prefix("@@ ")?;
    let (ranges, _) = body.split_once(" @@")?;
    let mut old_count = 0usize;
    let mut plus_range = None;
    for tok in ranges.split_whitespace() {
        if let Some(m) = tok.strip_prefix('-') {
            if let Some((_, c)) = parse_range_count(m) {
                old_count = c;
            }
        } else if let Some(p) = tok.strip_prefix('+') {
            plus_range = parse_range_count(p);
        }
    }
    let (start, count) = plus_range?;
    if count == 0 {
        Some((old_count, start, start, 0, true))
    } else {
        let s = start.max(1);
        let end = s.saturating_add(count.checked_sub(1)?);
        Some((old_count, s, end, count, false))
    }
}

pub fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let (_, start, end, _, is_deletion) = parse_hunk_header_counts(line)?;
    if is_deletion {
        let anchor = start.max(1);
        Some((anchor, anchor))
    } else {
        Some((start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_header_handles_single_line_multi_line_and_deletions() {
        assert_eq!(parse_hunk_header("@@ -10 +12 @@ fn foo"), Some((12, 12)));
        assert_eq!(parse_hunk_header("@@ -10,3 +20,5 @@"), Some((20, 24)));
        assert_eq!(parse_hunk_header("@@ -45,4 +44,0 @@"), Some((44, 44)));
    }

    #[test]
    fn diff_body_line_starting_with_triple_plus_never_spoofs_filename() {
        let diff = "\
diff --git a/src/real.ts b/src/real.ts
--- a/src/real.ts
+++ b/src/real.ts
@@ -1,0 +2,2 @@
+++ fake_file.ts
+const ok = 1;
@@ -10,0 +12,1 @@
+const ok2 = 2;
";
        let parsed = parse_unified_diff(diff);
        assert_eq!(
            parsed,
            vec![
                ("src/real.ts".to_string(), 2, 3),
                ("src/real.ts".to_string(), 12, 12),
            ]
        );
    }

    #[test]
    fn diff_records_deleted_and_renamed_files() {
        let diff = "\
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn gone() {}
-fn gone2() {}
diff --git a/src/old.rs b/src/new.rs
similarity index 100%
rename from src/old.rs
rename to src/new.rs
";
        let parsed = parse_unified_diff_full(diff);
        assert_eq!(parsed.deleted_files, vec!["src/gone.rs".to_string()]);
        assert_eq!(parsed.extra_touched, vec!["src/new.rs".to_string()]);
    }

    #[test]
    fn unquote_diff_path_unescapes_c_escapes() {
        assert_eq!(unquote_diff_path("\"src/a\\\"b.rs\""), "src/a\"b.rs");
        assert_eq!(unquote_diff_path("\"src/a\\\\b.rs\""), "src/a\\b.rs");
    }

    #[test]
    fn diff_parser_never_panics_on_utf8_octal_slice_or_usize_overflow() {
        let malicious = "+++ \"b/\\0aé.rs\"\n@@ -1,1 +18446744073709551615,2 @@\n";
        let parsed = parse_unified_diff(malicious);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].2, usize::MAX);
    }

    #[test]
    fn pure_deletion_overlap_matches_braceless_tail_and_ignores_line_zero() {
        let tail_del = DiffHunk {
            file: "a.py".into(),
            start: 2,
            end: 2,
            is_deletion: true,
        };
        assert!(hunk_overlaps_span(&tail_del, 1, 2));

        let top_del = DiffHunk {
            file: "a.py".into(),
            start: 0,
            end: 0,
            is_deletion: true,
        };
        assert!(!hunk_overlaps_span(&top_del, 1, 2));
    }

    #[test]
    fn staged_diff_uses_index_spans_and_emits_unverified_member_warning() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let st = Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(st.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub struct S;\nimpl S {\n    pub fn save_rows(&self) {\n        let a = 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.rs"),
            "pub fn run() {\n    let s = get_thing();\n    s.save_rows();\n}\n",
        )
        .unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "init",
        ]);

        // Stage a change inside S::save_rows (L4).
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub struct S;\nimpl S {\n    pub fn save_rows(&self) {\n        let a = 2;\n    }\n}\n",
        )
        .unwrap();
        run(&["add", "lib.rs"]);

        // Now shift working-tree lines by inserting an unrelated function at top (unstaged!).
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn unrelated_top() {\n    let x = 0;\n    let y = 1;\n    let z = 2;\n}\npub struct S;\nimpl S {\n    pub fn save_rows(&self) {\n        let a = 2;\n    }\n}\n",
        )
        .unwrap();

        let aliases = AliasSet::default();
        let mut syms = crate::parser::parse_file(
            Path::new("lib.rs"),
            &std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            &aliases,
        )
        .unwrap();
        syms.extend(
            crate::parser::parse_file(
                Path::new("app.rs"),
                &std::fs::read_to_string(dir.path().join("app.rs")).unwrap(),
                &aliases,
            )
            .unwrap(),
        );
        let graph = SymbolGraph::new(syms);
        let report = calculate_diff_blast(dir.path(), &graph, true, None, 3).unwrap();
        let mutated_names: Vec<&str> = report
            .mutated_symbols
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(mutated_names, vec!["S::save_rows"]);
        assert!(
            report
                .unverified_member_calls
                .as_deref()
                .is_some_and(|w| w.contains(".save_rows()")),
            "expected unverified member warning in diff report, got: {:?}",
            report.unverified_member_calls
        );
    }
}
