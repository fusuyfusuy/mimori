use crate::graph::blast::{traverse_multi_upstream, BlastNode};
use crate::graph::SymbolGraph;
use crate::memory::{check_debt, MemoryLedger};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(30);

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

fn run_git_diff_with_timeout(root: &Path, staged: bool, base: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args([
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
        ]);
    if staged {
        cmd.arg("--staged");
    }
    if let Some(b) = base {
        cmd.arg(b);
    }
    cmd.arg("--");

    let mut child = cmd.spawn().context("Failed to execute 'git diff'")?;
    let mut stdout_pipe = child.stdout.take().context("missing git stdout pipe")?;
    let mut stderr_pipe = child.stderr.take().context("missing git stderr pipe")?;

    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
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
                    bail!("git diff timed out after {}s", GIT_DIFF_TIMEOUT.as_secs());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    let stdout_bytes = out_handle.join().unwrap_or_default();
    let stderr_bytes = err_handle.join().unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        bail!("git diff exited with error: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&stdout_bytes).into_owned())
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

    let mut mutated_indices = Vec::new();
    let mut seen_mutated = HashSet::new();
    for h in &parsed.hunks {
        let Some(file_syms) = syms_by_file.get(h.file.as_str()) else {
            continue;
        };
        for &idx in file_syms {
            let sym = &graph.symbols[idx];
            let overlaps = if h.is_deletion {
                h.start > 0 && sym.start_line <= h.start && h.start < sym.end_line
            } else {
                sym.start_line <= h.end && sym.end_line >= h.start
            };
            if overlaps && seen_mutated.insert(idx) {
                mutated_indices.push(idx);
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

    let (affected, entry_points, test_suites, literal_test_mentions) =
        traverse_multi_upstream(graph, &mutated_indices, depth);

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
        affected,
        entry_points,
        test_suites,
        literal_test_mentions,
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
                b'0'..=b'7' if i + 3 < bytes.len() => {
                    let oct = &inner[i + 1..i + 4];
                    if let Ok(val) = u8::from_str_radix(oct, 8) {
                        out.push(val);
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
        Some((old_count, s, s + count - 1, count, false))
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
}
