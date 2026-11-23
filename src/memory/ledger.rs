use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const MAX_DEBT_CEILING: usize = 30;
const MAX_SCOPED_NOTES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtItem {
    pub what: String,
    pub why: String,
    pub trigger: String,
    pub raw: String,
    pub line_number: usize,
    pub is_accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintError {
    pub line_number: usize,
    pub line: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintReport {
    pub debt_count: usize,
    pub max_ceiling: usize,
    pub schema_errors: Vec<LintError>,
    pub strikethrough_errors: Vec<usize>,
    pub ceiling_breached: bool,
    pub passed: bool,
}

impl LintReport {
    pub fn to_m2m_output(&self) -> String {
        if self.passed {
            if self.ceiling_breached {
                format!(
                    "MEM_LINT_WARN: debt: {}/{} lines (soft ceiling exceeded); schema: valid; no_strikethrough: ok; exit 0.",
                    self.debt_count, self.max_ceiling
                )
            } else {
                format!(
                    "MEM_LINT: debt: {}/{} lines; schema: valid; no_strikethrough: ok; exit 0.",
                    self.debt_count, self.max_ceiling
                )
            }
        } else {
            let mut parts = Vec::new();
            if self.ceiling_breached {
                parts.push(format!(
                    "soft ceiling exceeded: {}/{} lines",
                    self.debt_count, self.max_ceiling
                ));
            }
            for err in &self.schema_errors {
                parts.push(format!(
                    "line {} malformed: {}",
                    err.line_number, err.reason
                ));
            }
            for line_no in &self.strikethrough_errors {
                parts.push(format!("line {} forbidden strikethrough/checkbox", line_no));
            }
            format!("MEM_LINT_FAIL: {}; exit 1.", parts.join("; "))
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryLedger {
    pub workspace_root: PathBuf,
    pub raw_content: String,
    pub epics: String,
    pub debt_items: Vec<DebtItem>,
    pub raw_debt_lines: Vec<(usize, String)>,
    pub vocab: String,
    pub gotchas: String,
    pub vocab_gotchas: String,
    pub debt_heading_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemorySection {
    Epics,
    Debt,
    Vocab,
    Gotchas,
    VocabGotchas,
    Other,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FenceState {
    ch: u8,
    count: usize,
}

/// Tracks CommonMark fenced code blocks (backticks `` ``` `` and tildes `~~~`,
/// up to 3 leading spaces). Returns `true` when the current line is inside or
/// delimits a fence.
pub(crate) fn update_code_fence(line: &str, state: &mut FenceState) -> bool {
    let bytes = line.as_bytes();
    let leading_spaces = bytes.iter().take_while(|&&b| b == b' ').count();
    if leading_spaces > 3 {
        return state.count > 0;
    }
    let rest = &line[leading_spaces..];
    let first = rest.as_bytes().first().copied().unwrap_or(0);

    if state.count == 0 {
        if first == b'`' || first == b'~' {
            let run = rest.bytes().take_while(|&b| b == first).count();
            let info = &rest[run..];
            if run >= 3 && (first == b'~' || !info.contains('`')) {
                state.ch = first;
                state.count = run;
                return true;
            }
        }
        false
    } else {
        if first == state.ch {
            let run = rest.bytes().take_while(|&b| b == state.ch).count();
            if run >= state.count && rest[run..].trim().is_empty() {
                state.ch = 0;
                state.count = 0;
            }
        }
        true
    }
}

fn has_heading_word(heading_lower: &str, words: &[&str]) -> bool {
    heading_lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|tok| words.contains(&tok))
}

pub(crate) fn classify_memory_heading(trimmed_line: &str) -> Option<MemorySection> {
    let trimmed = trimmed_line.trim();
    if let Some(rest) = trimmed.strip_prefix("## ") {
        let heading = rest.trim().to_lowercase();
        let has_epic = has_heading_word(&heading, &["epic", "epics"]);
        let has_debt = has_heading_word(&heading, &["debt"]);
        let has_vocab = has_heading_word(&heading, &["vocab", "vocabulary", "glossary"]);
        let has_gotcha = has_heading_word(&heading, &["gotcha", "gotchas", "trap", "traps"]);

        if has_epic {
            Some(MemorySection::Epics)
        } else if has_debt {
            Some(MemorySection::Debt)
        } else if has_vocab && has_gotcha {
            Some(MemorySection::VocabGotchas)
        } else if has_vocab {
            Some(MemorySection::Vocab)
        } else if has_gotcha {
            Some(MemorySection::Gotchas)
        } else {
            Some(MemorySection::Other)
        }
    } else if let Some(rest) = trimmed.strip_prefix("# ") {
        // Legacy instruction comment emitted inside `## KNOWN DEBT` starts with
        // `# Deliberate gaps...`; keep treating it as a comment rather than H1.
        if rest
            .trim_start()
            .to_lowercase()
            .starts_with("deliberate gaps")
        {
            None
        } else {
            Some(MemorySection::Other)
        }
    } else {
        None
    }
}

fn append_section(target: &mut String, text: &str) {
    if target.is_empty() {
        *target = text.to_string();
    } else {
        target.push_str("\n\n");
        target.push_str(text);
    }
}

fn flush_section(
    section: Option<MemorySection>,
    content: &str,
    epics: &mut String,
    vocab: &mut String,
    gotchas: &mut String,
    vocab_gotchas: &mut String,
) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return;
    }
    match section {
        Some(MemorySection::Epics) => append_section(epics, trimmed),
        Some(MemorySection::Vocab) => {
            append_section(vocab, trimmed);
            append_section(vocab_gotchas, trimmed);
        }
        Some(MemorySection::Gotchas) => {
            append_section(gotchas, trimmed);
            append_section(vocab_gotchas, trimmed);
        }
        Some(MemorySection::VocabGotchas) => {
            append_section(vocab, trimmed);
            append_section(gotchas, trimmed);
            append_section(vocab_gotchas, trimmed);
        }
        _ => {}
    }
}

pub(crate) fn confine_agents_path(workspace_root: &Path, rel: &str) -> Result<PathBuf> {
    let root = if workspace_root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        workspace_root
    };
    let canon_root = root
        .canonicalize()
        .with_context(|| format!("canonicalize workspace root {}", root.display()))?;

    let rel_path = Path::new(rel);
    if rel_path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        bail!("Path '{}' escapes workspace root", root.join(rel).display());
    }

    let agents_dir = root.join(".agents");
    if fs::symlink_metadata(&agents_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        match agents_dir.canonicalize() {
            Ok(canon_target) if canon_target.starts_with(&canon_root) => {}
            _ => bail!("Path '{}' escapes workspace root", agents_dir.display()),
        }
    }

    let target = root.join(rel_path);
    if target != agents_dir
        && fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink())
    {
        match target.canonicalize() {
            Ok(canon_target) if canon_target.starts_with(&canon_root) => {}
            _ => bail!("Path '{}' escapes workspace root", target.display()),
        }
    }

    if let Some(parent) = target.parent().filter(|p| fs::symlink_metadata(p).is_ok()) {
        let canon_parent = parent
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Path '{}' escapes workspace root", target.display()))?;
        if !canon_parent.starts_with(&canon_root) {
            bail!("Path '{}' escapes workspace root", target.display());
        }
    }

    Ok(target)
}

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn acquire_write_lock(dir: &Path) -> Result<LockGuard> {
    let lock_path = dir.join(".memory.lock");
    for _ in 0..50 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                return Ok(LockGuard(lock_path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(meta) = fs::metadata(&lock_path) {
                    if let Ok(modified) = meta.modified() {
                        if SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or(Duration::ZERO)
                            > Duration::from_secs(30)
                        {
                            let _ = fs::remove_file(&lock_path);
                            continue;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("lock {}", lock_path.display())),
        }
    }
    bail!(
        "Timed out acquiring memory lock '{}'; another mimori process may be writing",
        lock_path.display()
    );
}

impl MemoryLedger {
    pub fn memory_path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".agents").join("memory.md")
    }

    /// Scaffold `.agents/memory.md` (returns `true` if written).
    /// Architectural decisions belong in `docs/adr/*.md` (indexed by `dump`),
    /// avoiding a dead-letter `.agents/decisions.md` copy.
    pub fn scaffold(workspace_root: &Path, force: bool) -> Result<bool> {
        if crate::workspace::walker::is_disallowed_workspace_root(workspace_root) {
            bail!(
                "Refusing to scaffold memory in home or system directory '{}'",
                workspace_root.display()
            );
        }
        let agents_dir = confine_agents_path(workspace_root, ".agents")?;
        let mem_file = confine_agents_path(workspace_root, ".agents/memory.md")?;

        fs::create_dir_all(&agents_dir)
            .with_context(|| format!("create dir {}", agents_dir.display()))?;

        if !mem_file.exists() || force {
            let template = "\
# Project Memory

## Active Epics & Scale
- Scale: Baseline architecture initialized.

## KNOWN DEBT (open only — one line per item, delete when done)
<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->

## Domain Vocabulary & Gotchas
- [@scope: src/] Terms & empirical gotchas: add scoped notes via `mimori memory add`.
";
            Self::atomic_write(&mem_file, template)
                .with_context(|| format!("write {}", mem_file.display()))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn load(workspace_root: &Path) -> Result<Self> {
        if crate::workspace::walker::is_disallowed_workspace_root(workspace_root) {
            bail!(
                "Refusing to load memory in home or system directory '{}'",
                workspace_root.display()
            );
        }
        let path = confine_agents_path(workspace_root, ".agents/memory.md")?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read memory file {}", path.display()))?;
        Ok(Self::from_str(&content, workspace_root.to_path_buf()))
    }

    pub fn from_str(content: &str, workspace_root: PathBuf) -> Self {
        let mut epics = String::new();
        let mut vocab = String::new();
        let mut gotchas = String::new();
        let mut vocab_gotchas = String::new();
        let mut debt_items = Vec::new();
        let mut raw_debt_lines = Vec::new();
        let mut debt_heading_count = 0usize;

        let mut current_section: Option<MemorySection> = None;
        let mut section_content = String::new();
        let mut fence = FenceState::default();

        for (idx, line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();
            let in_fence = update_code_fence(line, &mut fence);

            if !in_fence {
                if let Some(next_section) = classify_memory_heading(trimmed) {
                    flush_section(
                        current_section,
                        &section_content,
                        &mut epics,
                        &mut vocab,
                        &mut gotchas,
                        &mut vocab_gotchas,
                    );
                    section_content.clear();
                    if next_section == MemorySection::Debt {
                        debt_heading_count += 1;
                    }
                    current_section = Some(next_section);
                    continue;
                }
            }

            if current_section == Some(MemorySection::Debt) {
                if !in_fence && !trimmed.is_empty() && !is_debt_comment_line(trimmed) {
                    raw_debt_lines.push((line_number, line.to_string()));
                    if let Ok(item) = parse_debt_line(line, line_number) {
                        debt_items.push(item);
                    }
                }
            } else if current_section.is_some() {
                section_content.push_str(line);
                section_content.push('\n');
            }
        }

        flush_section(
            current_section,
            &section_content,
            &mut epics,
            &mut vocab,
            &mut gotchas,
            &mut vocab_gotchas,
        );

        Self {
            workspace_root,
            raw_content: content.to_string(),
            epics,
            debt_items,
            raw_debt_lines,
            vocab,
            gotchas,
            vocab_gotchas,
            debt_heading_count,
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = confine_agents_path(&self.workspace_root, ".agents/memory.md")?;
        Self::atomic_write(&path, &self.raw_content)
    }

    pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let write_target = if let Some(parent) = path.parent() {
            if parent.file_name().is_some_and(|n| n == ".agents") {
                if let (Some(ws_root), Some(fname)) = (parent.parent(), path.file_name()) {
                    let rel = format!(".agents/{}", fname.to_string_lossy());
                    let confined = confine_agents_path(ws_root, &rel)?;
                    if fs::symlink_metadata(&confined).is_ok_and(|m| m.file_type().is_symlink()) {
                        confined.canonicalize().with_context(|| {
                            format!("resolve permitted symlink {}", confined.display())
                        })?
                    } else {
                        confined
                    }
                } else {
                    path.to_path_buf()
                }
            } else if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                bail!("Path '{}' escapes workspace root", path.display());
            } else {
                path.to_path_buf()
            }
        } else {
            path.to_path_buf()
        };

        let parent = write_target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let _lock = acquire_write_lock(parent)?;

        let mut tmp_path = None;
        let mut file = None;
        for _ in 0..32 {
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let candidate =
                write_target.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => {
                    tmp_path = Some(candidate);
                    file = Some(f);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("create temporary memory file {}", candidate.display())
                    });
                }
            }
        }

        let (tmp_path, mut file) = match (tmp_path, file) {
            (Some(p), Some(f)) => (p, f),
            _ => bail!("Failed to allocate unique temporary file for atomic write"),
        };

        if let Err(err) = file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(err)
                .with_context(|| format!("write temporary memory file {}", tmp_path.display()));
        }
        drop(file);

        if let Err(err) = fs::rename(&tmp_path, &write_target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err)
                .with_context(|| format!("atomic rename memory file {}", write_target.display()));
        }
        Ok(())
    }

    pub fn lint(&self) -> LintReport {
        let mut schema_errors = Vec::new();
        let mut strikethrough_errors = Vec::new();

        for (line_number, raw_line) in &self.raw_debt_lines {
            let trimmed = raw_line.trim_start();
            if raw_line.contains("~~") {
                strikethrough_errors.push(*line_number);
                schema_errors.push(LintError {
                    line_number: *line_number,
                    line: raw_line.clone(),
                    reason: "strikethrough '~~' forbidden (delete line when done)".to_string(),
                });
                continue;
            }
            if let Some(after_bullet) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                let after = after_bullet.trim_start();
                if after.starts_with("[x]") || after.starts_with("[X]") {
                    strikethrough_errors.push(*line_number);
                    schema_errors.push(LintError {
                        line_number: *line_number,
                        line: raw_line.clone(),
                        reason: "completed checkbox '- [x]' forbidden (delete line when done)"
                            .to_string(),
                    });
                    continue;
                }
                if after.starts_with("[ ]") {
                    schema_errors.push(LintError {
                        line_number: *line_number,
                        line: raw_line.clone(),
                        reason: "checkbox '- [ ]' forbidden (use standard bullet '- ')".to_string(),
                    });
                    continue;
                }
            }

            if let Err(err) = parse_debt_line(raw_line, *line_number) {
                schema_errors.push(err);
            }
        }

        let debt_count = self.raw_debt_lines.len();
        let ceiling_breached = debt_count > MAX_DEBT_CEILING;
        // Soft ceiling: warn above 30, fail only on schema or strikethrough/checkbox errors.
        let passed = schema_errors.is_empty() && strikethrough_errors.is_empty();

        LintReport {
            debt_count,
            max_ceiling: MAX_DEBT_CEILING,
            schema_errors,
            strikethrough_errors,
            ceiling_breached,
            passed,
        }
    }

    /// Surgically delete matching debt line(s) from `## KNOWN DEBT`.
    /// Rejects empty or delimiter-only patterns, errors on 0 matches, and
    /// refuses multi-line deletion unless `all == true`.
    pub fn resolve(&mut self, pattern: &str, all: bool) -> Result<Vec<String>> {
        let pattern_trimmed = pattern.trim();
        if pattern_trimmed.is_empty() {
            bail!("Cannot resolve debt item: pattern must not be empty");
        }
        if !pattern_trimmed.chars().any(|c| c.is_alphanumeric())
            || pattern_trimmed.chars().count() < 2
        {
            bail!(
                "Cannot resolve debt item: pattern '{}' is too broad; pass a specific debt substring",
                pattern_trimmed
            );
        }

        let pattern_lower = pattern_trimmed.to_lowercase();
        let lines: Vec<&str> = self.raw_content.lines().collect();
        let mut matched_indices: Vec<usize> = Vec::new();
        let mut in_debt_section = false;
        let mut fence = FenceState::default();

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if update_code_fence(line, &mut fence) {
                continue;
            }
            if let Some(section) = classify_memory_heading(trimmed) {
                in_debt_section = section == MemorySection::Debt;
                continue;
            }
            if !in_debt_section || trimmed.is_empty() || is_debt_comment_line(trimmed) {
                continue;
            }
            let haystack = match parse_debt_line(line, idx + 1) {
                Ok(item) => format!("{} {} {}", item.what, item.why, item.trigger).to_lowercase(),
                Err(_) => trimmed
                    .strip_prefix("- ")
                    .or_else(|| trimmed.strip_prefix("* "))
                    .unwrap_or(trimmed)
                    .to_lowercase(),
            };
            if haystack.contains(&pattern_lower) {
                matched_indices.push(idx);
            }
        }

        if matched_indices.is_empty() {
            bail!(
                "No debt item matched '{}'; run `mimori memory show --section debt` to inspect open items",
                pattern_trimmed
            );
        }
        if matched_indices.len() > 1 && !all {
            let preview: Vec<String> = matched_indices
                .iter()
                .take(5)
                .map(|&i| format!("  - {}", lines[i].trim()))
                .collect();
            bail!(
                "Pattern '{}' matched {} debt items (pass --all to delete all):\n{}",
                pattern_trimmed,
                matched_indices.len(),
                preview.join("\n")
            );
        }

        let delete_set: HashSet<usize> = matched_indices.iter().copied().collect();
        let deleted: Vec<String> = matched_indices
            .iter()
            .map(|&i| lines[i].trim().to_string())
            .collect();
        let new_lines: Vec<&str> = lines
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !delete_set.contains(i))
            .map(|(_, l)| l)
            .collect();

        let mut updated = new_lines.join("\n");
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        self.raw_content = updated;
        *self = Self::from_str(&self.raw_content, self.workspace_root.clone());
        self.save()?;

        Ok(deleted)
    }

    /// Append a structured entry to `.agents/memory.md` under `section`
    /// (`epics`, `debt`, `vocab`, `gotchas`), optionally anchored with
    /// `@scope:` or `@sym:` for JIT delivery in `slice`, `blast`, and `diff`.
    pub fn add_entry(
        &mut self,
        section: &str,
        text: &str,
        scope: Option<&str>,
        sym: Option<&str>,
    ) -> Result<String> {
        let clean_text = text.trim();
        if clean_text.is_empty() || clean_text.contains('\n') || clean_text.contains('\r') {
            bail!("Memory entry text must be a single non-empty line");
        }

        let sec_lower = section.trim().to_lowercase();
        let target_sec = if sec_lower.contains("epic") {
            MemorySection::Epics
        } else if sec_lower.contains("debt") {
            MemorySection::Debt
        } else if sec_lower.contains("vocab") || sec_lower.contains("glossary") {
            MemorySection::Vocab
        } else if sec_lower.contains("gotcha") || sec_lower.contains("trap") {
            MemorySection::Gotchas
        } else {
            bail!(
                "Unknown section '{}': expected 'epics', 'debt', 'vocab', or 'gotchas'",
                section
            );
        };

        let bullet = if target_sec == MemorySection::Debt {
            let candidate = if clean_text.starts_with("- ") || clean_text.starts_with("* ") {
                clean_text.to_string()
            } else {
                format!("- {}", clean_text)
            };
            parse_debt_line(&candidate, 1)
                .map_err(|e| anyhow::anyhow!("Invalid debt entry: {}", e.reason))?;
            candidate
        } else {
            let mut tags = Vec::new();
            if let Some(sc) = scope.map(str::trim).filter(|s| !s.is_empty()) {
                tags.push(format!("@scope: {}", sc));
            }
            if let Some(sm) = sym.map(str::trim).filter(|s| !s.is_empty()) {
                tags.push(format!("@sym: {}", sm));
            }
            let body = clean_text
                .strip_prefix("- ")
                .or_else(|| clean_text.strip_prefix("* "))
                .unwrap_or(clean_text);
            if tags.is_empty() {
                format!("- {}", body)
            } else {
                format!("- [{}] {}", tags.join(", "), body)
            }
        };

        let lines: Vec<&str> = self.raw_content.lines().collect();
        let mut insert_at: Option<usize> = None;
        let mut current: Option<MemorySection> = None;
        let mut fence = FenceState::default();

        let matches_target = |sec: MemorySection| match target_sec {
            MemorySection::Vocab => {
                matches!(sec, MemorySection::Vocab | MemorySection::VocabGotchas)
            }
            MemorySection::Gotchas => {
                matches!(sec, MemorySection::Gotchas | MemorySection::VocabGotchas)
            }
            other => sec == other,
        };

        for (idx, line) in lines.iter().enumerate() {
            if update_code_fence(line, &mut fence) {
                continue;
            }
            if let Some(next_sec) = classify_memory_heading(line) {
                if current.is_some_and(matches_target) && insert_at.is_none() {
                    insert_at = Some(idx);
                }
                current = Some(next_sec);
            }
        }
        if current.is_some_and(matches_target) && insert_at.is_none() {
            insert_at = Some(lines.len());
        }

        let mut out: Vec<String> = lines.into_iter().map(str::to_string).collect();
        if let Some(mut pos) = insert_at {
            while pos > 0 && out[pos - 1].trim().is_empty() {
                pos -= 1;
            }
            out.insert(pos, bullet.clone());
        } else {
            if !out.last().is_some_and(|l| l.trim().is_empty()) {
                out.push(String::new());
            }
            let heading = match target_sec {
                MemorySection::Epics => "## Active Epics & Scale",
                MemorySection::Debt => {
                    "## KNOWN DEBT (open only — one line per item, delete when done)"
                }
                MemorySection::Vocab | MemorySection::Gotchas | MemorySection::VocabGotchas => {
                    "## Domain Vocabulary & Gotchas"
                }
                MemorySection::Other => "## Notes",
            };
            out.push(heading.to_string());
            out.push(bullet.clone());
        }

        let mut updated = out.join("\n");
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        self.raw_content = updated;
        *self = Self::from_str(&self.raw_content, self.workspace_root.clone());
        self.save()?;

        Ok(bullet)
    }

    /// Return JIT memory notes anchored via `@scope:` or `@sym:` matching any
    /// of the queried `files` or `symbols`. Deduped, capped at `MAX_SCOPED_NOTES`.
    pub fn scoped_notes(&self, files: &[&str], symbols: &[&str]) -> Vec<String> {
        if files.is_empty() && symbols.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut fence = FenceState::default();

        for line in self.raw_content.lines() {
            if update_code_fence(line, &mut fence) {
                continue;
            }
            let trimmed = line.trim();
            let Some(body) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            else {
                continue;
            };
            if !body.contains("@scope:") && !body.contains("@sym:") {
                continue;
            }
            let (scopes, syms) = extract_note_anchors(body);
            let file_hit = scopes
                .iter()
                .any(|pat| files.iter().any(|f| scope_anchor_matches(pat, f)));
            let sym_hit = syms
                .iter()
                .any(|target| symbols.iter().any(|s| sym_anchor_matches(target, s)));

            if (file_hit || sym_hit) && seen.insert(body.to_string()) {
                out.push(body.to_string());
                if out.len() >= MAX_SCOPED_NOTES {
                    break;
                }
            }
        }
        out
    }

    /// Best-effort workspace lookup for JIT scoped memory notes (used by
    /// `slice`, `blast`, `diff`). Returns empty if no `.agents/memory.md` exists.
    pub fn scoped_notes_for_workspace(
        workspace_root: &Path,
        files: &[&str],
        symbols: &[&str],
    ) -> Vec<String> {
        if files.is_empty() && symbols.is_empty() {
            return Vec::new();
        }
        let Ok(mem_path) = confine_agents_path(workspace_root, ".agents/memory.md") else {
            return Vec::new();
        };
        if !mem_path.is_file() {
            return Vec::new();
        }
        let Ok(ledger) = Self::load(workspace_root) else {
            return Vec::new();
        };
        ledger.scoped_notes(files, symbols)
    }

    pub fn get_section(&self, section: &str) -> Option<String> {
        let sec = section.to_lowercase();
        if sec.contains("epic") {
            (!self.epics.is_empty()).then(|| self.epics.clone())
        } else if sec.contains("debt") {
            if self.raw_debt_lines.is_empty() {
                None
            } else {
                let mut out = String::new();
                for (_, line) in &self.raw_debt_lines {
                    out.push_str(line);
                    out.push('\n');
                }
                Some(out.trim_end().to_string())
            }
        } else if sec.contains("vocab") || sec.contains("glossary") {
            if !self.vocab.is_empty() {
                Some(self.vocab.clone())
            } else {
                (!self.vocab_gotchas.is_empty()).then(|| self.vocab_gotchas.clone())
            }
        } else if sec.contains("gotcha") || sec.contains("trap") {
            if !self.gotchas.is_empty() {
                Some(self.gotchas.clone())
            } else {
                (!self.vocab_gotchas.is_empty()).then(|| self.vocab_gotchas.clone())
            }
        } else {
            None
        }
    }
}

/// Index `docs/adr/*.md` H1 titles for compact Turn-0 `dump` inclusion.
pub fn index_adr_titles(workspace_root: &Path) -> Vec<(String, String)> {
    let Ok(adr_dir) =
        crate::workspace::walker::confine_to_workspace(workspace_root, Path::new("docs/adr"))
    else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&adr_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();
    files.sort();

    let mut out = Vec::new();
    for path in files.into_iter().take(20) {
        let Ok(confined) = crate::workspace::walker::confine_to_workspace(workspace_root, &path)
        else {
            continue;
        };
        let Ok(content) = fs::read_to_string(&confined) else {
            continue;
        };
        let fname = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if fname.eq_ignore_ascii_case("README.md") {
            continue;
        }
        let title = content
            .lines()
            .find_map(|l| l.trim().strip_prefix("# ").map(str::trim))
            .unwrap_or(&fname)
            .to_string();
        out.push((format!("docs/adr/{}", fname), title));
    }
    out
}

fn is_debt_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with('#') || trimmed.starts_with("<!--")
}

fn extract_note_anchors(body: &str) -> (Vec<String>, Vec<String>) {
    let mut scopes = Vec::new();
    let mut syms = Vec::new();
    // Parse `@scope:` and `@sym:` anchors whether bracketed `[@scope: a, @sym: b]`
    // or inline `- @scope:src/db/ Never ...` (with or without space after `:`).
    for (tag, out) in [("@scope:", &mut scopes), ("@sym:", &mut syms)] {
        let mut rest = body;
        while let Some(pos) = rest.find(tag) {
            let after = &rest[pos + tag.len()..];
            let trimmed = after.trim_start_matches([' ', '\t']);
            let token_end = trimmed
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ']' | ')'))
                .unwrap_or(trimmed.len());
            let raw = trimmed[..token_end]
                .trim_matches('`')
                .trim_end_matches([':', ']', ')']);
            let val = if raw.len() > 1 {
                raw.trim_end_matches('.')
            } else {
                raw
            };
            if !val.is_empty() {
                out.push(val.to_string());
            }
            rest = &trimmed[token_end..];
        }
    }
    (scopes, syms)
}

fn scope_anchor_matches(pattern: &str, file: &str) -> bool {
    let pat = pattern.trim().trim_start_matches("./").replace('\\', "/");
    let rel = file.trim().trim_start_matches("./").replace('\\', "/");
    if pat.is_empty() || pat == "*" || pat == "." {
        return true;
    }
    if pat.contains('*') {
        return simple_glob_match(&pat, &rel);
    }
    let pat = pat.trim_end_matches('/');
    rel == pat
        || rel
            .strip_prefix(pat)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn simple_glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            pi += 1;
            star_t = ti;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn sym_anchor_matches(target: &str, sym: &str) -> bool {
    let t = target.trim();
    let s = sym.trim();
    if t.is_empty() || s.is_empty() {
        return false;
    }
    if t == s {
        return true;
    }
    let s_leaf = s.rsplit([':', '.']).next().unwrap_or(s);
    if !t.contains("::") && !t.contains('.') && t == s_leaf {
        return true;
    }
    s.ends_with(&format!("::{t}")) || s.ends_with(&format!(".{t}"))
}

fn is_accepted_prefix(what: &str) -> bool {
    let w = what.trim();
    let lower = w.to_lowercase();
    if lower == "accepted" || lower == "[accepted]" {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("[accepted]") {
        return rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace);
    }
    if let Some(rest) = lower.strip_prefix("accepted") {
        return rest.starts_with(':') || rest.starts_with(char::is_whitespace);
    }
    false
}

pub fn parse_debt_line(raw: &str, line_number: usize) -> Result<DebtItem, LintError> {
    let trimmed = raw.trim_start();
    let content = if let Some(s) = trimmed.strip_prefix("- ") {
        s
    } else if let Some(s) = trimmed.strip_prefix("* ") {
        s
    } else {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "missing bullet prefix '- '".to_string(),
        });
    };

    let Some(left_idx) = content.find("<-") else {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "missing '<-' reason separator".to_string(),
        });
    };

    let what = content[..left_idx].trim();
    let remainder = &content[left_idx + 2..];

    let Some(right_idx) = remainder.find("->") else {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "missing '->' trigger separator".to_string(),
        });
    };

    let why = remainder[..right_idx].trim();
    let trigger = remainder[right_idx + 2..].trim();

    if what.is_empty() {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "empty 'what' description".to_string(),
        });
    }
    if why.is_empty() {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "empty 'why/ceiling' explanation".to_string(),
        });
    }
    if trigger.is_empty() {
        return Err(LintError {
            line_number,
            line: raw.to_string(),
            reason: "empty 'upgrade_trigger' target".to_string(),
        });
    }

    Ok(DebtItem {
        what: what.to_string(),
        why: why.to_string(),
        trigger: trigger.to_string(),
        raw: raw.to_string(),
        line_number,
        is_accepted: is_accepted_prefix(what),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_classifier_uses_word_boundaries_and_h1_terminates_sections() {
        assert_eq!(
            classify_memory_heading("## Bootstrap Notes"),
            Some(MemorySection::Other)
        );
        assert_eq!(
            classify_memory_heading("## Gotchas"),
            Some(MemorySection::Gotchas)
        );
        assert_eq!(
            classify_memory_heading("## Domain Vocabulary"),
            Some(MemorySection::Vocab)
        );
        assert_eq!(
            classify_memory_heading("## Domain Vocabulary & Gotchas"),
            Some(MemorySection::VocabGotchas)
        );
        assert_eq!(
            classify_memory_heading("# Appendix"),
            Some(MemorySection::Other)
        );
        assert_eq!(
            classify_memory_heading("# Deliberate gaps get ledger lines: - accepted ..."),
            None
        );
    }

    #[test]
    fn lint_allows_brackets_inside_debt_prose_and_checks_word_boundary_accepted() {
        let md = "\
# Project Memory

## KNOWN DEBT (open only)
- arr[x] index unchecked <- len < 100 -> add bounds check
- accepted_types missing <- v1 -> add enum
";
        let ledger = MemoryLedger::from_str(md, PathBuf::from("."));
        let report = ledger.lint();
        assert!(
            report.passed,
            "unexpected errors: {:?}",
            report.schema_errors
        );
        assert!(!ledger.debt_items[1].is_accepted);
    }

    #[test]
    fn tilde_fences_and_scoped_notes_work() {
        let md = "\
# Project Memory

## Domain Vocabulary & Gotchas
- [@scope: src/graph/*] Graph edges drop cross-file untyped members.
- [@sym: MemoryLedger::lint] Soft 30-line ceiling warns instead of failing CI.
~~~markdown
## KNOWN DEBT
- fake debt inside tilde fence
~~~
";
        let ledger = MemoryLedger::from_str(md, PathBuf::from("."));
        assert!(ledger.raw_debt_lines.is_empty());
        let notes = ledger.scoped_notes(&["src/graph/blast.rs"], &[]);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("cross-file untyped members"));

        let sym_notes = ledger.scoped_notes(&[], &["MemoryLedger::lint"]);
        assert_eq!(sym_notes.len(), 1);
        assert!(sym_notes[0].contains("Soft 30-line ceiling"));
    }
}
