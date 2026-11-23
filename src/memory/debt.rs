use super::ledger::{
    classify_memory_heading, confine_agents_path, update_code_fence, FenceState, MemoryLedger,
    MemorySection, MAX_DEBT_CEILING,
};
use crate::workspace::walker::{is_disallowed_workspace_root, WorkspaceScope};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

const MAX_DEBT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AST_DEPTH: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InCodeMarker {
    pub file: String,
    pub line: usize,
    pub what: String,
    pub ceiling: String,
    pub trigger: String,
    pub valid_trigger: bool,
    pub error_reason: Option<String>,
    pub raw: String,
}

#[derive(Clone, Copy)]
enum CommentStyle {
    TreeSitter(TsLang),
    HashLine,
    SqlDash,
    CStyle,
}

#[derive(Clone, Copy)]
enum TsLang {
    Rust,
    TypeScript,
    Tsx,
    Python,
    Go,
}

fn comment_style_for(path: &Path) -> Option<CommentStyle> {
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if fname.eq_ignore_ascii_case("dockerfile") || fname.starts_with("Dockerfile.") {
        return Some(CommentStyle::HashLine);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(CommentStyle::TreeSitter(TsLang::Rust)),
        "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => {
            Some(CommentStyle::TreeSitter(TsLang::TypeScript))
        }
        "tsx" | "jsx" => Some(CommentStyle::TreeSitter(TsLang::Tsx)),
        "py" => Some(CommentStyle::TreeSitter(TsLang::Python)),
        "go" => Some(CommentStyle::TreeSitter(TsLang::Go)),
        "sql" => Some(CommentStyle::SqlDash),
        "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml" | "ini" | "conf" | "mk" | "pyi" => {
            Some(CommentStyle::HashLine)
        }
        "css" | "scss" | "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "kt" | "swift" => {
            Some(CommentStyle::CStyle)
        }
        _ => None,
    }
}

fn ts_language(lang: TsLang) -> Language {
    match lang {
        TsLang::Rust => tree_sitter_rust::LANGUAGE.into(),
        TsLang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        TsLang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        TsLang::Python => tree_sitter_python::LANGUAGE.into(),
        TsLang::Go => tree_sitter_go::LANGUAGE.into(),
    }
}

fn has_marker_keyword(text: &str) -> bool {
    text.as_bytes()
        .windows(8)
        .any(|w| w.eq_ignore_ascii_case(b"mimodept") || w.eq_ignore_ascii_case(b"mimodebt"))
}

/// Extract a marker payload from the body of a real comment line.
/// Drops legacy `ponytail:`; supports `mimodept:` (and typo `mimodebt:`).
fn parse_marker_from_comment_text(
    comment_line: &str,
    is_block_comment: bool,
    file: &str,
    line_no: usize,
    raw_line: &str,
) -> Option<InCodeMarker> {
    if !has_marker_keyword(comment_line) {
        return None;
    }

    let bytes = comment_line.as_bytes();
    let mut payload_start = None;
    for i in 0..bytes.len() {
        let prefix_len = if bytes[i..].len() >= 9
            && (bytes[i..i + 9].eq_ignore_ascii_case(b"mimodept:")
                || bytes[i..i + 9].eq_ignore_ascii_case(b"mimodebt:"))
        {
            9
        } else {
            continue;
        };
        let boundary_ok =
            i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'/' | b'*' | b'#' | b'-');
        if boundary_ok {
            payload_start = Some(i + prefix_len);
            break;
        }
    }

    let start = payload_start?;
    let mut slice = &comment_line[start..];
    if is_block_comment {
        if let Some(end_idx) = slice.rfind("*/") {
            slice = &slice[..end_idx];
        }
    }
    let slice = slice.trim();

    let Some(left_idx) = slice.find("<-") else {
        return Some(InCodeMarker {
            file: file.to_string(),
            line: line_no,
            what: slice.to_string(),
            ceiling: String::new(),
            trigger: String::new(),
            valid_trigger: false,
            error_reason: Some("missing '<-' separator".into()),
            raw: raw_line.to_string(),
        });
    };

    let what = slice[..left_idx].trim();
    let rest = &slice[left_idx + 2..];

    let Some(right_idx) = rest.find("->") else {
        return Some(InCodeMarker {
            file: file.to_string(),
            line: line_no,
            what: what.to_string(),
            ceiling: rest.trim().to_string(),
            trigger: String::new(),
            valid_trigger: false,
            error_reason: Some("missing '->' trigger separator".into()),
            raw: raw_line.to_string(),
        });
    };

    let ceiling = rest[..right_idx].trim();
    let trigger = rest[right_idx + 2..].trim();

    let (valid_trigger, error_reason) = if what.is_empty() {
        (false, Some("empty 'what'".into()))
    } else if ceiling.is_empty() {
        (false, Some("empty 'ceiling'".into()))
    } else if trigger.is_empty() {
        (false, Some("empty 'trigger'".into()))
    } else {
        (true, None)
    };

    Some(InCodeMarker {
        file: file.to_string(),
        line: line_no,
        what: what.to_string(),
        ceiling: ceiling.to_string(),
        trigger: trigger.to_string(),
        valid_trigger,
        error_reason,
        raw: raw_line.to_string(),
    })
}

fn collect_ts_comments(
    node: Node,
    source: &str,
    lines: &[&str],
    file: &str,
    depth: usize,
    out: &mut Vec<InCodeMarker>,
) {
    if depth > MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if kind == "line_comment" || kind == "block_comment" || kind == "comment" {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            if has_marker_keyword(text) {
                let is_block = text.starts_with("/*");
                let start_row = node.start_position().row;
                for (offset, seg) in text.lines().enumerate() {
                    let line_no = start_row + offset + 1;
                    let raw = lines.get(start_row + offset).copied().unwrap_or(seg);
                    if let Some(m) =
                        parse_marker_from_comment_text(seg, is_block, file, line_no, raw)
                    {
                        out.push(m);
                    }
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_comments(child, source, lines, file, depth + 1, out);
    }
}

fn scan_content_markers(content: &str, file: &str, style: CommentStyle) -> Vec<InCodeMarker> {
    if !has_marker_keyword(content) {
        return Vec::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    match style {
        CommentStyle::TreeSitter(lang) => {
            let mut parser = Parser::new();
            if parser.set_language(&ts_language(lang)).is_err() {
                return Vec::new();
            }
            let Some(tree) = parser.parse(content, None) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            collect_ts_comments(tree.root_node(), content, &lines, file, 0, &mut out);
            out
        }
        CommentStyle::HashLine => scan_line_prefix_comments(&lines, file, "#", false),
        CommentStyle::SqlDash => scan_line_prefix_comments(&lines, file, "--", false),
        CommentStyle::CStyle => {
            let mut out = scan_line_prefix_comments(&lines, file, "//", false);
            out.extend(scan_line_prefix_comments(&lines, file, "/*", true));
            out
        }
    }
}

fn scan_line_prefix_comments(
    lines: &[&str],
    file: &str,
    prefix: &str,
    is_block: bool,
) -> Vec<InCodeMarker> {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(m) = parse_marker_from_comment_text(rest, is_block, file, idx + 1, line) {
                out.push(m);
            }
        }
    }
    out
}

/// Public single-line helper preserved for callers/tests inspecting comment lines directly.
pub fn parse_mimodept_line(line: &str, file: &str, line_no: usize) -> Option<InCodeMarker> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("/*") {
        parse_marker_from_comment_text(rest, true, file, line_no, line)
    } else if let Some(rest) = trimmed
        .strip_prefix("//")
        .or_else(|| trimmed.strip_prefix('#'))
        .or_else(|| trimmed.strip_prefix("--"))
    {
        parse_marker_from_comment_text(rest, false, file, line_no, line)
    } else {
        None
    }
}

pub fn scan_debt_markers_checked(
    root: &Path,
    scope: Option<&str>,
) -> Result<(Vec<InCodeMarker>, Vec<String>)> {
    if is_disallowed_workspace_root(root) {
        bail!(
            "Refusing to scan debt in home or system directory '{}'",
            root.display()
        );
    }
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let scan_root = match scope {
        Some(sc) => {
            let parsed = WorkspaceScope::parse(&canon_root, sc, true)?;
            parsed.canon_path.unwrap_or_else(|| canon_root.clone())
        }
        None => canon_root.clone(),
    };

    let mut builder = crate::workspace::walker::walk_builder(&scan_root);
    builder.hidden(false).follow_links(false);
    if scan_root != canon_root {
        let root_ignore = canon_root.join(".mimoriignore");
        if root_ignore.is_file() {
            let _ = builder.add_ignore(root_ignore);
        }
    }

    let mut files = Vec::new();
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(&canon_root) else {
            continue;
        };
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md")
                    || ext.eq_ignore_ascii_case("markdown")
                    || ext.eq_ignore_ascii_case("mdx")
            })
        {
            continue;
        }
        let is_ignored_dir = crate::workspace::walker::is_ignored_rel(rel)
            || rel
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new(".agents"));
        if is_ignored_dir {
            continue;
        }
        let Some(style) = comment_style_for(path) else {
            continue;
        };
        let rel_str = crate::workspace::walker::normalize_rel_str(rel);
        files.push((path.to_path_buf(), rel_str, style));
    }

    enum FileScan {
        Ok(Vec<InCodeMarker>),
        Err(String),
    }

    let results: Vec<FileScan> = files
        .into_par_iter()
        .map(|(full_path, rel_path, style)| {
            let Ok(meta) = fs::metadata(&full_path) else {
                return FileScan::Err(format!("{}: cannot stat", rel_path));
            };
            if meta.len() > MAX_DEBT_FILE_BYTES {
                return FileScan::Err(format!("{}: exceeds 8 MiB scan cap", rel_path));
            }
            match fs::read_to_string(&full_path) {
                Ok(content) => FileScan::Ok(scan_content_markers(&content, &rel_path, style)),
                Err(e) => FileScan::Err(format!("{}: {}", rel_path, e)),
            }
        })
        .collect();

    let mut markers = Vec::new();
    let mut read_errors = Vec::new();
    for res in results {
        match res {
            FileScan::Ok(ms) => markers.extend(ms),
            FileScan::Err(e) => read_errors.push(e),
        }
    }

    markers.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    read_errors.sort();
    Ok((markers, read_errors))
}

pub fn scan_debt_markers(root: &Path, scope: Option<&str>) -> Vec<InCodeMarker> {
    scan_debt_markers_checked(root, scope)
        .map(|(m, _)| m)
        .unwrap_or_default()
}

pub fn list_debt(root: &Path, scope: Option<&str>) -> Result<(Vec<InCodeMarker>, String)> {
    let (markers, read_errors) = scan_debt_markers_checked(root, scope)?;
    if markers.is_empty() && read_errors.is_empty() {
        return Ok((markers, "DEBT_SCAN: 0 markers found; exit 0.".to_string()));
    }

    let mut unique_files = HashSet::new();
    for m in &markers {
        unique_files.insert(&m.file);
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "DEBT_SCAN: {} markers found across {} files:",
        markers.len(),
        unique_files.len()
    ));

    for m in &markers {
        if m.valid_trigger {
            lines.push(format!(
                "- {}:{}: {} <- {} -> {}",
                m.file, m.line, m.what, m.ceiling, m.trigger
            ));
        } else {
            let reason = m.error_reason.as_deref().unwrap_or("invalid");
            lines.push(format!(
                "- {}:{}: {} <- {} -> {} [{}: FAIL]",
                m.file, m.line, m.what, m.ceiling, m.trigger, reason
            ));
        }
    }
    for err in &read_errors {
        lines.push(format!("- WARN (unreadable): {}", err));
    }

    Ok((markers, lines.join("\n")))
}

fn strip_accepted_prefix(what: &str) -> &str {
    let w = what.trim();
    if w.len() >= 10 && w.as_bytes()[..10].eq_ignore_ascii_case(b"[accepted]") {
        let rest = &w[10..];
        return rest.strip_prefix(':').unwrap_or(rest).trim_start();
    }
    if w.len() >= 8 && w.as_bytes()[..8].eq_ignore_ascii_case(b"accepted") {
        let rest = &w[8..];
        if let Some(after_colon) = rest.strip_prefix(':') {
            return after_colon.trim_start();
        }
        let trimmed = rest.trim_start();
        if trimmed.len() < rest.len() {
            return trimmed;
        }
    }
    w
}

fn normalize_debt_key(what: &str, ceiling: &str, trigger: &str) -> String {
    format!(
        "{} <- {} -> {}",
        strip_accepted_prefix(what),
        ceiling.trim(),
        trigger.trim()
    )
}

pub fn check_debt(root: &Path) -> (bool, String) {
    let (markers, read_errors) = match scan_debt_markers_checked(root, None) {
        Ok(pair) => pair,
        Err(e) => {
            return (false, format!("DEBT_CHECK_FAIL: {}; exit 1.", e));
        }
    };

    let total_markers = markers.len();
    let valid_markers: Vec<_> = markers.iter().filter(|m| m.valid_trigger).collect();
    let valid_count = valid_markers.len();

    let valid_keys: HashSet<String> = valid_markers
        .iter()
        .map(|m| normalize_debt_key(&m.what, &m.ceiling, &m.trigger))
        .collect();

    let manual_count = match confine_agents_path(root, ".agents/memory.md") {
        Ok(mem_path) if mem_path.exists() => match MemoryLedger::load(root) {
            Ok(ledger) => {
                let mut seen_manual = HashSet::new();
                ledger
                    .debt_items
                    .iter()
                    .filter(|d| d.is_accepted)
                    .filter(|d| {
                        let key = normalize_debt_key(&d.what, &d.why, &d.trigger);
                        !valid_keys.contains(&key) && seen_manual.insert(key)
                    })
                    .count()
            }
            Err(e) => {
                return (
                    false,
                    format!("DEBT_CHECK_FAIL: load .agents/memory.md: {}; exit 1.", e),
                );
            }
        },
        Ok(_) => 0,
        Err(e) => {
            return (false, format!("DEBT_CHECK_FAIL: {}; exit 1.", e));
        }
    };

    let total_debt = valid_keys.len() + manual_count;
    let ceiling_breached = total_debt > MAX_DEBT_CEILING;
    let has_invalid_triggers = valid_count < total_markers;
    let passed = !has_invalid_triggers && read_errors.is_empty();

    let output = if passed {
        if ceiling_breached {
            format!(
                "DEBT_CHECK_WARN: markers: {}; valid_triggers: {}/{}; soft ceiling exceeded: {}/{}; exit 0.",
                total_markers, valid_count, total_markers, total_debt, MAX_DEBT_CEILING
            )
        } else {
            format!(
                "DEBT_CHECK: markers: {}; valid_triggers: {}/{}; ceiling: {}/{}; exit 0.",
                total_markers, valid_count, total_markers, total_debt, MAX_DEBT_CEILING
            )
        }
    } else {
        let mut reasons = Vec::new();
        if has_invalid_triggers {
            if let Some(first_fail) = markers.iter().find(|m| !m.valid_trigger) {
                let reason = first_fail
                    .error_reason
                    .as_deref()
                    .unwrap_or("missing trigger");
                reasons.push(format!(
                    "valid_triggers: {}/{} ({}:{} {})",
                    valid_count, total_markers, first_fail.file, first_fail.line, reason
                ));
            }
        }
        if !read_errors.is_empty() {
            reasons.push(format!("unreadable files: {}", read_errors.join(", ")));
        }
        if ceiling_breached {
            reasons.push(format!(
                "soft ceiling exceeded: {}/{}",
                total_debt, MAX_DEBT_CEILING
            ));
        }
        format!(
            "DEBT_CHECK_FAIL: markers: {}; {}; exit 1.",
            total_markers,
            reasons.join("; ")
        )
    };

    (passed, output)
}

pub fn sync_debt(root: &Path) -> Result<(usize, usize, usize, String)> {
    let (markers, read_errors) = scan_debt_markers_checked(root, None)?;
    if !read_errors.is_empty() {
        bail!(
            "Refusing to sync debt with unreadable source files: {}",
            read_errors.join(", ")
        );
    }

    let valid_markers: Vec<_> = markers.iter().filter(|m| m.valid_trigger).collect();

    let mem_path = confine_agents_path(root, ".agents/memory.md")?;
    if !mem_path.exists() {
        MemoryLedger::scaffold(root, false)?;
    }

    let ledger = MemoryLedger::load(root)?;
    if ledger.debt_heading_count > 1 {
        bail!(
            "Refusing to sync debt: .agents/memory.md contains {} debt headings; consolidate to one `## KNOWN DEBT` section",
            ledger.debt_heading_count
        );
    }

    let valid_keys: HashSet<String> = valid_markers
        .iter()
        .map(|m| normalize_debt_key(&m.what, &m.ceiling, &m.trigger))
        .collect();

    let mut manual_items: Vec<String> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    for item in &ledger.debt_items {
        let key = normalize_debt_key(&item.what, &item.why, &item.trigger);
        if item.is_accepted && !valid_keys.contains(&key) && seen_keys.insert(key) {
            manual_items.push(format!(
                "- {} <- {} -> {}",
                item.what, item.why, item.trigger
            ));
        }
    }

    let mut combined_lines = manual_items.clone();
    for m in &valid_markers {
        let key = normalize_debt_key(&m.what, &m.ceiling, &m.trigger);
        if seen_keys.insert(key) {
            combined_lines.push(format!("- {} <- {} -> {}", m.what, m.ceiling, m.trigger));
        }
    }

    // Never truncate waivers or markers silently — preserve all items and warn above 30.
    let synced_count = combined_lines.len();
    let ceiling_breached = synced_count > MAX_DEBT_CEILING;

    let pruned: Vec<String> = ledger
        .debt_items
        .iter()
        .filter(|d| !d.is_accepted)
        .map(|d| format!("- {} <- {} -> {}", d.what, d.why, d.trigger))
        .filter(|l| !combined_lines.contains(l))
        .collect();

    let updated_raw = replace_debt_section(&ledger.raw_content, &combined_lines);
    MemoryLedger::atomic_write(&mem_path, &updated_raw)?;

    let in_code_count = valid_markers.len();
    let manual_count = manual_items.len();

    let output = if ceiling_breached {
        format!(
            "DEBT_SYNC_WARN: soft ceiling exceeded ({}/{} lines, all preserved); in_code: {}; manual: {}; synced to .agents/memory.md: {}/{} lines; exit 0.",
            synced_count, MAX_DEBT_CEILING, in_code_count, manual_count, synced_count, MAX_DEBT_CEILING
        )
    } else {
        format!(
            "DEBT_SYNC: in_code: {}; manual: {}; synced to .agents/memory.md: {}/{} lines; exit 0.",
            in_code_count, manual_count, synced_count, MAX_DEBT_CEILING
        )
    };

    let output = if pruned.is_empty() {
        output
    } else {
        format!(
            "{} PRUNED {} ledger line(s) with no in-code marker and no `accepted` prefix: {}",
            output,
            pruned.len(),
            pruned.join(" | ")
        )
    };

    Ok((in_code_count, manual_count, synced_count, output))
}

pub fn replace_debt_section(content: &str, new_debt_lines: &[String]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();

    let mut in_debt_section = false;
    let mut debt_inserted = false;
    let mut fence = FenceState::default();

    for line in lines {
        let trimmed = line.trim();
        let in_fence = update_code_fence(line, &mut fence);

        if !in_fence {
            if let Some(section) = classify_memory_heading(trimmed) {
                if section == MemorySection::Debt && !debt_inserted {
                    in_debt_section = true;
                    out.push(line.to_string());
                    out.push(
                        "<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->"
                            .to_string(),
                    );
                    out.push(String::new());
                    if !new_debt_lines.is_empty() {
                        for debt in new_debt_lines {
                            out.push(debt.clone());
                        }
                        out.push(String::new());
                    }
                    debt_inserted = true;
                    continue;
                } else {
                    in_debt_section = false;
                }
            }
        }

        if in_debt_section {
            continue;
        }

        out.push(line.to_string());
    }

    if !debt_inserted {
        out.push(String::new());
        out.push("## KNOWN DEBT (open only — one line per item, delete when done)".to_string());
        out.push("<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->".to_string());
        out.push(String::new());
        for debt in new_debt_lines {
            out.push(debt.clone());
        }
    }

    let mut res = out.join("\n");
    if !res.ends_with('\n') {
        res.push('\n');
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treesitter_scanner_ignores_strings_raw_strings_and_ponytail() {
        let src = r##"
fn demo() {
    let s = "// mimodept: fake inside string <- cap -> trigger";
    let r = r#"// mimodept: fake inside raw string <- cap -> trigger"#;
    // ponytail: dropped legacy syntax <- cap -> trigger
    // mimodept: real glob */foo <- cap 1 -> trigger 1
}
"##;
        let found =
            scan_content_markers(src, "src/demo.rs", CommentStyle::TreeSitter(TsLang::Rust));
        assert_eq!(found.len(), 1, "got: {found:?}");
        assert_eq!(found[0].what, "real glob */foo");
        assert!(found[0].valid_trigger);
    }

    #[test]
    fn treesitter_python_ignores_docstrings_and_extracts_comments() {
        let src = r##"
def demo():
    """
    # mimodept: fake inside docstring <- cap -> trigger
    """
    x = "# mimodept: fake inside string <- cap -> trigger"
    # mimodept: real py debt <- cap 2 -> trigger 2
    return x
"##;
        let found = scan_content_markers(src, "app.py", CommentStyle::TreeSitter(TsLang::Python));
        assert_eq!(found.len(), 1, "got: {found:?}");
        assert_eq!(found[0].what, "real py debt");
    }

    #[test]
    fn replace_debt_section_preserves_trailing_h1_appendix() {
        let md = "\
# Project Memory

## KNOWN DEBT (open only)
- old <- a -> b

# Appendix
Keep this safe.
";
        let out = replace_debt_section(md, &["- new <- c -> d".to_string()]);
        assert!(out.contains("- new <- c -> d"));
        assert!(!out.contains("- old <- a -> b"));
        assert!(out.contains("# Appendix"));
        assert!(out.contains("Keep this safe."));
    }
}
