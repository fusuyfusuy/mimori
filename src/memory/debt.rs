use crate::memory::ledger::{MemoryLedger, MAX_DEBT_CEILING};
use anyhow::Result;
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

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

pub fn parse_ponytail_line(line: &str, file: &str, line_no: usize) -> Option<InCodeMarker> {
    if !line
        .as_bytes()
        .windows(8)
        .any(|w| w.eq_ignore_ascii_case(b"ponytail"))
    {
        return None;
    }

    let bytes = line.as_bytes();
    let is_rust = file.ends_with(".rs");
    let prefixes = [
        "# ponytail:",
        "// ponytail:",
        "/* ponytail:",
        "-- ponytail:",
    ];

    let mut in_double_quote = false;
    let mut in_single_quote = false;
    let mut in_backtick = false;
    let mut escaped = false;

    let mut found_pos: Option<(usize, &str)> = None;

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];

        if escaped {
            escaped = false;
            i += 1;
            continue;
        }

        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }

        if b == b'"' && !in_single_quote && !in_backtick {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }

        // In Rust, single quotes are char literals (never multiline or containing comments) or lifetimes ('a).
        // Avoid letting lifetimes like 'a toggle in_single_quote.
        if !is_rust && b == b'\'' && !in_double_quote && !in_backtick {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }

        if b == b'`' && !in_double_quote && !in_single_quote {
            in_backtick = !in_backtick;
            i += 1;
            continue;
        }

        if !in_double_quote && !in_single_quote && !in_backtick {
            for prefix in &prefixes {
                if bytes[i..].len() >= prefix.len()
                    && bytes[i..i + prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
                {
                    found_pos = Some((i, *prefix));
                    break;
                }
            }
            if found_pos.is_some() {
                break;
            }
        }

        i += 1;
    }

    let (pos, prefix) = found_pos?;
    let start = pos + prefix.len();
    let mut slice = &line[start..];

    if let Some(end_idx) = slice.rfind("*/") {
        slice = &slice[..end_idx];
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
            raw: line.to_string(),
        });
    };

    let what = slice[..left_idx].trim();
    let rest = &slice[left_idx + 2..];

    let Some(right_idx) = rest.find("->") else {
        let ceiling = rest.trim();
        return Some(InCodeMarker {
            file: file.to_string(),
            line: line_no,
            what: what.to_string(),
            ceiling: ceiling.to_string(),
            trigger: String::new(),
            valid_trigger: false,
            error_reason: Some("missing '->' trigger separator".into()),
            raw: line.to_string(),
        });
    };

    let ceiling = rest[..right_idx].trim();
    let trigger = rest[right_idx + 2..].trim();

    let (valid, error_reason) = if what.is_empty() {
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
        valid_trigger: valid,
        error_reason,
        raw: line.to_string(),
    })
}

pub fn scan_debt_markers(root: &Path, scope: Option<&str>) -> Vec<InCodeMarker> {
    let scan_root = match scope {
        Some(s) => {
            let joined = root.join(s);
            let Ok(canon_root) = root.canonicalize() else {
                return Vec::new();
            };
            let Ok(canon_joined) = joined.canonicalize() else {
                return Vec::new();
            };
            if !canon_joined.starts_with(&canon_root) {
                return Vec::new();
            }
            joined
        }
        None => root.to_path_buf(),
    };
    if !scan_root.exists() {
        return Vec::new();
    }

    let mut builder = WalkBuilder::new(&scan_root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);

    let mut files = Vec::new();
    for entry in builder.build().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy();
        if rel_str.ends_with(".md") || rel_str.ends_with(".markdown") {
            continue;
        }
        let is_ignored_dir = rel.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            matches!(
                s.as_ref(),
                ".git" | ".mimori" | "target" | "node_modules" | "dist" | "build" | ".agents"
            )
        });
        if is_ignored_dir {
            continue;
        }
        files.push((path.to_path_buf(), rel_str.to_string()));
    }

    let mut markers: Vec<InCodeMarker> = files
        .into_par_iter()
        .flat_map(|(full_path, rel_path)| {
            let Ok(content) = fs::read_to_string(&full_path) else {
                return Vec::new();
            };
            let mut file_markers = Vec::new();
            for (line_idx, line) in content.lines().enumerate() {
                if let Some(marker) = parse_ponytail_line(line, &rel_path, line_idx + 1) {
                    file_markers.push(marker);
                }
            }
            file_markers
        })
        .collect();

    markers.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    markers
}

pub fn list_debt(root: &Path, scope: Option<&str>) -> (Vec<InCodeMarker>, String) {
    let markers = scan_debt_markers(root, scope);
    if markers.is_empty() {
        return (markers, "DEBT_SCAN: 0 markers found; exit 0.".to_string());
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

    let output = lines.join("\n");
    (markers, output)
}

pub fn check_debt(root: &Path) -> (bool, String) {
    let markers = scan_debt_markers(root, None);
    let total_markers = markers.len();
    let valid_count = markers.iter().filter(|m| m.valid_trigger).count();

    let manual_count = if let Ok(ledger) = MemoryLedger::load(root) {
        ledger.debt_items.iter().filter(|d| d.is_accepted).count()
    } else {
        0
    };

    let total_debt = valid_count + manual_count;
    let ceiling_breached = total_debt > MAX_DEBT_CEILING;
    let has_invalid_triggers = valid_count < total_markers;

    let passed = !ceiling_breached && !has_invalid_triggers;

    let output = if passed {
        format!(
            "DEBT_CHECK: markers: {}; valid_triggers: {}/{}; ceiling: {}/{}; exit 0.",
            total_markers, valid_count, total_markers, total_debt, MAX_DEBT_CEILING
        )
    } else {
        let mut reasons = Vec::new();
        if has_invalid_triggers {
            let first_fail = markers.iter().find(|m| !m.valid_trigger).unwrap();
            let reason = first_fail
                .error_reason
                .as_deref()
                .unwrap_or("missing trigger");
            reasons.push(format!(
                "valid_triggers: {}/{} ({}:{} {})",
                valid_count, total_markers, first_fail.file, first_fail.line, reason
            ));
        }
        if ceiling_breached {
            reasons.push(format!(
                "ceiling breached: {}/{}; markers: {}; manual: {}",
                total_debt, MAX_DEBT_CEILING, valid_count, manual_count
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
    let markers = scan_debt_markers(root, None);
    let valid_markers: Vec<_> = markers.iter().filter(|m| m.valid_trigger).collect();

    if !MemoryLedger::memory_path(root).exists() {
        MemoryLedger::scaffold(root, false)?;
    }

    let ledger = MemoryLedger::load(root)?;
    let mut manual_items: Vec<String> = Vec::new();

    for item in &ledger.debt_items {
        if item.is_accepted {
            manual_items.push(format!(
                "- {} <- {} -> {}",
                item.what, item.why, item.trigger
            ));
        }
    }

    let mut combined_lines = Vec::new();
    for manual in &manual_items {
        combined_lines.push(manual.clone());
    }

    for m in &valid_markers {
        let line = format!("- {} <- {} -> {}", m.what, m.ceiling, m.trigger);
        if !combined_lines.contains(&line) {
            combined_lines.push(line);
        }
    }

    let total_before_ceiling = combined_lines.len();
    let ceiling_breached = total_before_ceiling > MAX_DEBT_CEILING;
    if ceiling_breached {
        combined_lines.truncate(MAX_DEBT_CEILING);
    }

    let raw = fs::read_to_string(MemoryLedger::memory_path(root))?;
    let updated_raw = replace_debt_section(&raw, &combined_lines);
    MemoryLedger::atomic_write(&MemoryLedger::memory_path(root), &updated_raw)?;

    let in_code_count = valid_markers.len();
    let manual_count = manual_items.len();
    let synced_count = combined_lines.len();

    let output = if ceiling_breached {
        format!(
            "DEBT_SYNC_WARN: ceiling breached: {}/{} lines (truncated to 30); in_code: {}; manual: {}; synced to .agents/memory.md: {}/{} lines; exit 0.",
            total_before_ceiling, MAX_DEBT_CEILING, in_code_count, manual_count, synced_count, MAX_DEBT_CEILING
        )
    } else {
        format!(
            "DEBT_SYNC: in_code: {}; manual: {}; synced to .agents/memory.md: {}/{} lines; exit 0.",
            in_code_count, manual_count, synced_count, MAX_DEBT_CEILING
        )
    };

    Ok((in_code_count, manual_count, synced_count, output))
}

pub fn replace_debt_section(content: &str, new_debt_lines: &[String]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();

    let mut in_debt_section = false;
    let mut debt_inserted = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            let heading = trimmed.trim_start_matches('#').trim().to_lowercase();
            if heading.contains("known debt") || heading.contains("debt") {
                in_debt_section = true;
                out.push(line.to_string());
                out.push(
                    "# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>"
                        .to_string(),
                );
                out.push(String::new());
                for debt in new_debt_lines {
                    out.push(debt.clone());
                }
                out.push(String::new());
                debt_inserted = true;
                continue;
            } else {
                in_debt_section = false;
            }
        }

        if in_debt_section {
            // skip existing debt lines and comments under debt section
            continue;
        }

        out.push(line.to_string());
    }

    if !debt_inserted {
        out.push(String::new());
        out.push("## KNOWN DEBT (open only — one line per item, delete when done)".to_string());
        out.push(
            "# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>"
                .to_string(),
        );
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
