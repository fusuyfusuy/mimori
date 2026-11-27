use super::ledger::{
    classify_memory_heading, confine_agents_path, strip_accepted_prefix, update_code_fence,
    FenceState, MemoryLedger, MemorySection, MAX_DEBT_CEILING,
};
use crate::workspace::walker::{is_disallowed_workspace_root, WorkspaceScope};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

const MAX_DEBT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_DEBT_COMMENT: &str =
    "<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->";

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
        let boundary_ok = i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'/' | b'*' | b'#' | b'-' | b'!'
            );
        if boundary_ok {
            payload_start = Some(i + prefix_len);
            break;
        }
    }

    let start = payload_start?;
    let mut slice = comment_line[start..].trim();
    // Only strip a genuine closing `*/` at the end of the comment line; never
    // `rfind("*/")` mid-line, which corrupts glob patterns like `src/*/foo.rs`.
    if is_block_comment {
        if let Some(stripped) = slice.strip_suffix("*/") {
            slice = stripped.trim_end();
        }
    }

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

fn extract_comment_node_markers(
    node: Node,
    source: &str,
    lines: &[&str],
    file: &str,
    out: &mut Vec<InCodeMarker>,
) {
    let Ok(text) = node.utf8_text(source.as_bytes()) else {
        return;
    };
    if !has_marker_keyword(text) {
        return;
    }
    let is_block = text.starts_with("/*");
    let start_row = node.start_position().row;
    for (offset, seg) in text.lines().enumerate() {
        let line_no = start_row + offset + 1;
        let raw = lines.get(start_row + offset).copied().unwrap_or(seg);
        if let Some(m) = parse_marker_from_comment_text(seg, is_block, file, line_no, raw) {
            out.push(m);
        }
    }
}

/// Iterative TreeCursor pre-order traversal so arbitrarily deep ASTs never
/// stack-overflow or silently drop markers past an arbitrary depth cap.
fn collect_ts_comments(
    root: Node,
    source: &str,
    lines: &[&str],
    file: &str,
    out: &mut Vec<InCodeMarker>,
) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let is_comment = kind == "line_comment" || kind == "block_comment" || kind == "comment";

        if is_comment {
            extract_comment_node_markers(node, source, lines, file, out);
        } else if cursor.goto_first_child() {
            continue;
        }

        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn push_comment_segment(
    seg: &str,
    is_block: bool,
    file: &str,
    line_no: usize,
    lines: &[&str],
    out: &mut Vec<InCodeMarker>,
) {
    let raw = lines.get(line_no.saturating_sub(1)).copied().unwrap_or(seg);
    if let Some(m) = parse_marker_from_comment_text(seg, is_block, file, line_no, raw) {
        out.push(m);
    }
}

fn scan_hash_comments(content: &str, lines: &[&str], file: &str) -> Vec<InCodeMarker> {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut quote: u8 = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if quote != 0 {
                if b == b'\\' && quote == b'"' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == quote {
                    quote = 0;
                }
                i += 1;
                continue;
            }
            if b == b'"' || b == b'\'' {
                quote = b;
                i += 1;
            } else if b == b'#' {
                push_comment_segment(&line[i + 1..], false, file, idx + 1, lines, &mut out);
                break;
            } else {
                i += 1;
            }
        }
    }
    let _ = content;
    out
}

fn scan_c_or_sql_comments(
    content: &str,
    lines: &[&str],
    file: &str,
    sql_mode: bool,
) -> Vec<InCodeMarker> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line_no = 1usize;
    let mut quote: u8 = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if b == b'\n' {
                line_no += 1;
                if !sql_mode {
                    quote = 0;
                }
                i += 1;
                continue;
            }
            if sql_mode && b == quote && i + 1 < bytes.len() && bytes[i + 1] == quote {
                i += 2;
                continue;
            }
            if !sql_mode && b == b'\\' && i + 1 < bytes.len() {
                if bytes[i + 1] == b'\n' {
                    line_no += 1;
                }
                i += 2;
                continue;
            }
            if b == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }

        if b == b'\n' {
            line_no += 1;
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = b;
            i += 1;
            continue;
        }

        let is_line_comment = if sql_mode {
            b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-'
        } else {
            b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/'
        };
        if is_line_comment {
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'\n' {
                end += 1;
            }
            let seg = content[start..end].trim_end_matches('\r');
            push_comment_segment(seg, false, file, line_no, lines, &mut out);
            i = end;
            continue;
        }

        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            let mut seg_start = i;
            let mut seg_line = line_no;
            while i < bytes.len() {
                if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    let seg = content[seg_start..i].trim_end_matches('\r');
                    push_comment_segment(seg, true, file, seg_line, lines, &mut out);
                    i += 2;
                    break;
                }
                if bytes[i] == b'\n' {
                    let seg = content[seg_start..i].trim_end_matches('\r');
                    push_comment_segment(seg, true, file, seg_line, lines, &mut out);
                    line_no += 1;
                    i += 1;
                    seg_start = i;
                    seg_line = line_no;
                    continue;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    out
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
            collect_ts_comments(tree.root_node(), content, &lines, file, &mut out);
            out
        }
        CommentStyle::HashLine => scan_hash_comments(content, &lines, file),
        CommentStyle::SqlDash => scan_c_or_sql_comments(content, &lines, file, true),
        CommentStyle::CStyle => scan_c_or_sql_comments(content, &lines, file, false),
    }
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

enum FileScan {
    Ok(Vec<InCodeMarker>),
    Err(String),
}

fn collect_debt_scan_files(
    canon_root: &Path,
    scan_root: &Path,
) -> Vec<(PathBuf, String, CommentStyle)> {
    let mut builder = crate::workspace::walker::walk_builder(scan_root);
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
        let Ok(rel) = path.strip_prefix(canon_root) else {
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
    files
}

fn scan_single_file(full_path: &Path, rel_path: &str, style: CommentStyle) -> FileScan {
    let Ok(meta) = fs::metadata(full_path) else {
        return FileScan::Err(format!("{}: cannot stat", rel_path));
    };
    if meta.len() > MAX_DEBT_FILE_BYTES {
        return FileScan::Err(format!("{}: exceeds 8 MiB scan cap", rel_path));
    }
    let Ok(file) = File::open(full_path) else {
        return FileScan::Err(format!("{}: cannot open", rel_path));
    };
    let mut buf = Vec::new();
    if let Err(e) = file.take(MAX_DEBT_FILE_BYTES + 1).read_to_end(&mut buf) {
        return FileScan::Err(format!("{}: {}", rel_path, e));
    }
    if buf.len() as u64 > MAX_DEBT_FILE_BYTES {
        return FileScan::Err(format!("{}: exceeds 8 MiB scan cap", rel_path));
    }
    match String::from_utf8(buf) {
        Ok(content) => FileScan::Ok(scan_content_markers(&content, rel_path, style)),
        Err(e) => FileScan::Err(format!("{}: {}", rel_path, e)),
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

    let files = collect_debt_scan_files(&canon_root, &scan_root);
    let results: Vec<FileScan> = files
        .into_par_iter()
        .map(|(full_path, rel_path, style)| scan_single_file(&full_path, &rel_path, style))
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

fn normalize_debt_key(what: &str, ceiling: &str, trigger: &str) -> String {
    let clean_what = strip_accepted_prefix(what).unwrap_or_else(|| what.trim());
    format!("{} <- {} -> {}", clean_what, ceiling.trim(), trigger.trim())
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

    MemoryLedger::with_locked_ledger(root, |ledger| {
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

        let mut accepted_by_key: HashMap<String, String> = HashMap::new();
        let mut manual_items: Vec<String> = Vec::new();
        let mut seen_keys: HashSet<String> = HashSet::new();

        for item in &ledger.debt_items {
            if !item.is_accepted {
                continue;
            }
            let key = normalize_debt_key(&item.what, &item.why, &item.trigger);
            let formatted = format!("- {} <- {} -> {}", item.what, item.why, item.trigger);
            accepted_by_key
                .entry(key.clone())
                .or_insert_with(|| formatted.clone());
            if !valid_keys.contains(&key) && seen_keys.insert(key) {
                manual_items.push(formatted);
            }
        }

        let mut combined_lines = manual_items.clone();
        for m in &valid_markers {
            let key = normalize_debt_key(&m.what, &m.ceiling, &m.trigger);
            if seen_keys.insert(key.clone()) {
                if let Some(accepted_line) = accepted_by_key.get(&key) {
                    combined_lines.push(accepted_line.clone());
                } else {
                    combined_lines.push(format!("- {} <- {} -> {}", m.what, m.ceiling, m.trigger));
                }
            }
        }

        // Never truncate waivers or markers silently — preserve all items and warn above 30.
        let synced_count = combined_lines.len();
        let ceiling_breached = synced_count > MAX_DEBT_CEILING;

        let pruned: Vec<String> = ledger
            .debt_items
            .iter()
            .filter(|d| !d.is_accepted)
            .filter(|d| {
                let key = normalize_debt_key(&d.what, &d.why, &d.trigger);
                !seen_keys.contains(&key)
            })
            .map(|d| format!("- {} <- {} -> {}", d.what, d.why, d.trigger))
            .collect();

        ledger.raw_content = replace_debt_section(&ledger.raw_content, &combined_lines);

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
    })
}

/// Extract non-debt comments (`<!-- ... -->`, `# ...`), subheadings (`### ...`),
/// and fenced blocks from an existing `## KNOWN DEBT` section body so `debt sync`
/// preserves author notes instead of wiping them.
fn collect_preserved_debt_notes(section_lines: &[&str]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut fence = FenceState::default();
    let mut in_html_comment = false;

    for &line in section_lines {
        let trimmed = line.trim();
        if update_code_fence(line, &mut fence) {
            kept.push(line.to_string());
            continue;
        }
        if in_html_comment {
            kept.push(line.to_string());
            if trimmed.contains("-->") {
                in_html_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("<!--") {
            kept.push(line.to_string());
            if !trimmed.contains("-->") {
                in_html_comment = true;
            }
            continue;
        }
        if trimmed.starts_with('#') {
            kept.push(line.to_string());
        }
    }
    kept
}

fn push_debt_section_body(
    out: &mut Vec<String>,
    existing_section_lines: &[&str],
    new_debt_lines: &[String],
) {
    let preserved = collect_preserved_debt_notes(existing_section_lines);
    let has_deliberate_hint = preserved
        .iter()
        .any(|l| l.to_lowercase().contains("deliberate gaps"));

    if !has_deliberate_hint {
        out.push(DEFAULT_DEBT_COMMENT.to_string());
    }
    for note in preserved {
        out.push(note);
    }
    out.push(String::new());
    if !new_debt_lines.is_empty() {
        for debt in new_debt_lines {
            out.push(debt.clone());
        }
        out.push(String::new());
    }
}

pub fn replace_debt_section(content: &str, new_debt_lines: &[String]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();

    let mut debt_start: Option<usize> = None;
    let mut debt_end: Option<usize> = None;
    let mut fence = FenceState::default();

    for (idx, &line) in lines.iter().enumerate() {
        if update_code_fence(line, &mut fence) {
            continue;
        }
        if let Some(section) = classify_memory_heading(line) {
            if debt_start.is_none() && section == MemorySection::Debt {
                debt_start = Some(idx);
            } else if debt_start.is_some() && debt_end.is_none() {
                debt_end = Some(idx);
            }
        }
    }

    if let Some(start) = debt_start {
        let end = debt_end.unwrap_or(lines.len());
        for &line in &lines[..=start] {
            out.push(line.to_string());
        }
        push_debt_section_body(&mut out, &lines[start + 1..end], new_debt_lines);
        for &line in &lines[end..] {
            out.push(line.to_string());
        }
    } else {
        for &line in &lines {
            out.push(line.to_string());
        }
        out.push(String::new());
        out.push("## KNOWN DEBT (open only — one line per item, delete when done)".to_string());
        out.push(DEFAULT_DEBT_COMMENT.to_string());
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
    use tempfile::tempdir;

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
    fn block_comment_glob_and_bang_doc_comment_preserved() {
        let src = "\
/*
 * mimodept: glob src/*/foo.rs <- O(n) -> cache
 */
//!mimodept: crate doc debt <- cap 1 -> refactor
pub fn f() {}
";
        let found = scan_content_markers(src, "src/lib.rs", CommentStyle::TreeSitter(TsLang::Rust));
        assert_eq!(found.len(), 2, "got: {found:?}");
        assert_eq!(found[0].what, "glob src/*/foo.rs");
        assert!(found[0].valid_trigger);
        assert_eq!(found[1].what, "crate doc debt");
        assert!(found[1].valid_trigger);
    }

    #[test]
    fn non_ts_scanners_handle_multiline_and_inline_comments() {
        let c_src = "\
const char *fake = \"// mimodept: ignored <- 1 -> 2\";
int x = 1; // mimodept: inline c debt <- 1 -> 2
/*
 * mimodept: multiline c glob src/*.c <- O(n) -> cache
 */
";
        let c_found = scan_content_markers(c_src, "app.c", CommentStyle::CStyle);
        assert_eq!(c_found.len(), 2, "got: {c_found:?}");
        assert_eq!(c_found[0].what, "inline c debt");
        assert_eq!(c_found[1].what, "multiline c glob src/*.c");

        let sql_src = "\
SELECT '-- mimodept: ignored <- 1 -> 2';
SELECT 1 -- mimodept: inline sql debt <- 100 rows -> index
/* mimodept: block sql debt <- full scan -> partition */
";
        let sql_found = scan_content_markers(sql_src, "q.sql", CommentStyle::SqlDash);
        assert_eq!(sql_found.len(), 2, "got: {sql_found:?}");
        assert_eq!(sql_found[0].what, "inline sql debt");
        assert_eq!(sql_found[1].what, "block sql debt");

        let yaml_src = "\
url: \"http://example.com#mimodept: ignored <- 1 -> 2\"
timeout: 30 # mimodept: inline yaml debt <- fixed -> env var
";
        let yaml_found = scan_content_markers(yaml_src, "cfg.yaml", CommentStyle::HashLine);
        assert_eq!(yaml_found.len(), 1, "got: {yaml_found:?}");
        assert_eq!(yaml_found[0].what, "inline yaml debt");
    }

    #[test]
    fn deeply_nested_ast_marker_is_not_dropped() {
        let depth = 150;
        let mut src = String::from("pub fn deep() {\n");
        for _ in 0..depth {
            src.push_str("{ ");
        }
        src.push_str("\n// mimodept: deep marker <- depth 150 -> flatten\n");
        for _ in 0..depth {
            src.push_str("} ");
        }
        src.push_str("\n}\n");

        let found =
            scan_content_markers(&src, "src/deep.rs", CommentStyle::TreeSitter(TsLang::Rust));
        assert_eq!(found.len(), 1, "deeply nested marker must be found");
        assert_eq!(found[0].what, "deep marker");
    }

    #[test]
    fn sync_debt_preserves_accepted_waivers_and_section_comments() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".agents")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "// mimodept: lock <- 1 -> N\npub fn f() {}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".agents/memory.md"),
            "\
# Project Memory

## KNOWN DEBT
<!-- Team review note: audit every Friday -->
### Concurrency
- accepted lock <- 1 -> N
",
        )
        .unwrap();

        sync_debt(dir.path()).unwrap();
        let synced = fs::read_to_string(dir.path().join(".agents/memory.md")).unwrap();
        assert!(
            synced.contains("- accepted lock <- 1 -> N"),
            "accepted prefix must be preserved: {synced}"
        );
        assert!(
            synced.contains("<!-- Team review note: audit every Friday -->"),
            "custom HTML comment must be preserved: {synced}"
        );
        assert!(
            synced.contains("### Concurrency"),
            "subheading must be preserved: {synced}"
        );
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
