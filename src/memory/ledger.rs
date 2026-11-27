use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_DEBT_CEILING: usize = 30;
const MAX_SCOPED_NOTES: usize = 8;
pub(crate) const MAX_MEMORY_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

const DEFAULT_MEMORY_TEMPLATE: &str = "\
# Project Memory

## Active Epics & Scale
- Scale: Baseline architecture initialized.

## KNOWN DEBT (open only — one line per item, delete when done)
<!-- Deliberate gaps: - accepted <what> <- <why> -> <trigger> -->

## Domain Vocabulary & Gotchas
- [@scope: src/] Terms & empirical gotchas: add scoped notes via `mimori memory add`.
";

thread_local! {
    static HELD_LOCK_KEYS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

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

/// Classify a raw Markdown line as a memory section heading.
/// Per CommonMark, ATX headings allow at most 3 leading spaces (4+ spaces or
/// a leading tab is an indented code block and returns `None`).
pub(crate) fn classify_memory_heading(line: &str) -> Option<MemorySection> {
    let leading_spaces = line.bytes().take_while(|&b| b == b' ').count();
    if leading_spaces > 3 {
        return None;
    }
    let rest_line = &line[leading_spaces..];
    if rest_line.starts_with('\t') {
        return None;
    }
    let trimmed = rest_line.trim_end();

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
    ensure_confined_symlink(&agents_dir, &canon_root)?;

    let target = root.join(rel_path);
    if target != agents_dir {
        ensure_confined_symlink(&target, &canon_root)?;
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

fn ensure_confined_symlink(path: &Path, canon_root: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(());
    }
    match path.canonicalize() {
        Ok(canon_target) if canon_target.starts_with(canon_root) => Ok(()),
        _ => bail!("Path '{}' escapes workspace root", path.display()),
    }
}

fn read_to_string_bounded(path: &Path, max_bytes: u64) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    if buf.len() as u64 > max_bytes {
        bail!(
            "File '{}' exceeds maximum allowed size ({} bytes)",
            path.display(),
            max_bytes
        );
    }
    String::from_utf8(buf).with_context(|| format!("decode UTF-8 in {}", path.display()))
}

enum LockGuard {
    Owned {
        _file: File,
        path: PathBuf,
        key: PathBuf,
    },
    Reentrant,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let LockGuard::Owned { path, key, .. } = self {
            // Unlink while holding the exclusive OS lock; `_file` drops right after.
            let _ = fs::remove_file(path);
            HELD_LOCK_KEYS.with(|set| {
                set.borrow_mut().remove(key);
            });
        }
    }
}

fn lock_key_for_dir(dir: &Path) -> PathBuf {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .join(".memory.lock")
}

fn is_lock_held(dir: &Path) -> bool {
    let key = lock_key_for_dir(dir);
    HELD_LOCK_KEYS.with(|set| set.borrow().contains(&key))
}

fn reject_symlink_lock(lock_path: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(lock_path) {
        if meta.file_type().is_symlink() {
            bail!(
                "Path '{}' is a symlink; refusing to open lock file",
                lock_path.display()
            );
        }
    }
    Ok(())
}

fn verify_locked_inode(lock_path: &Path, file: &File) -> Result<bool> {
    let Ok(path_meta) = fs::symlink_metadata(lock_path) else {
        return Ok(false);
    };
    if path_meta.file_type().is_symlink() {
        bail!(
            "Path '{}' is a symlink; refusing to lock",
            lock_path.display()
        );
    }
    let file_meta = file
        .metadata()
        .with_context(|| format!("stat lock {}", lock_path.display()))?;
    if !file_meta.file_type().is_file() {
        bail!("Lock path '{}' is not a regular file", lock_path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(path_meta.dev() == file_meta.dev() && path_meta.ino() == file_meta.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}

fn acquire_write_lock(dir: &Path) -> Result<LockGuard> {
    let key = lock_key_for_dir(dir);
    if HELD_LOCK_KEYS.with(|set| set.borrow().contains(&key)) {
        return Ok(LockGuard::Reentrant);
    }

    let lock_path = dir.join(".memory.lock");
    for _ in 0..64 {
        reject_symlink_lock(&lock_path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open lock {}", lock_path.display()))?;
        file.lock()
            .with_context(|| format!("lock {}", lock_path.display()))?;
        if verify_locked_inode(&lock_path, &file)? {
            HELD_LOCK_KEYS.with(|set| {
                set.borrow_mut().insert(key.clone());
            });
            return Ok(LockGuard::Owned {
                _file: file,
                path: lock_path,
                key,
            });
        }
    }
    bail!(
        "Failed to acquire stable OS file lock on '{}'",
        lock_path.display()
    );
}

fn resolve_confined_write_target(path: &Path) -> Result<PathBuf> {
    let Some(parent) = path.parent() else {
        return Ok(path.to_path_buf());
    };
    if parent.file_name().is_some_and(|n| n == ".agents") {
        let (Some(ws_root), Some(fname)) = (parent.parent(), path.file_name()) else {
            return Ok(path.to_path_buf());
        };
        let rel = format!(".agents/{}", fname.to_string_lossy());
        let confined = confine_agents_path(ws_root, &rel)?;
        if fs::symlink_metadata(&confined).is_ok_and(|m| m.file_type().is_symlink()) {
            return confined
                .canonicalize()
                .with_context(|| format!("resolve permitted symlink {}", confined.display()));
        }
        return Ok(confined);
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("Path '{}' escapes workspace root", path.display());
    }
    Ok(path.to_path_buf())
}

fn create_unique_temp_file(write_target: &Path) -> Result<(PathBuf, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..32 {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = write_target.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("create temporary memory file {}", candidate.display())
                });
            }
        }
    }
    bail!("Failed to allocate unique temporary file for atomic write");
}

fn sync_parent_dir(parent: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        match File::open(parent) {
            Ok(dir_file) => {
                if let Err(err) = dir_file.sync_all() {
                    if !matches!(
                        err.kind(),
                        std::io::ErrorKind::InvalidInput
                            | std::io::ErrorKind::Unsupported
                            | std::io::ErrorKind::PermissionDenied
                    ) {
                        return Err(err).with_context(|| {
                            format!("fsync parent directory {}", parent.display())
                        });
                    }
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("open parent directory {}", parent.display()));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

enum DebtLineViolation {
    Strikethrough(LintError),
    CheckedBox(LintError),
    Schema(LintError),
}

impl DebtLineViolation {
    fn into_error(self) -> LintError {
        match self {
            Self::Strikethrough(e) | Self::CheckedBox(e) | Self::Schema(e) => e,
        }
    }
}

fn validate_debt_line(raw_line: &str, line_number: usize) -> Result<DebtItem, DebtLineViolation> {
    let trimmed = raw_line.trim_start();
    if raw_line.contains("~~") {
        return Err(DebtLineViolation::Strikethrough(LintError {
            line_number,
            line: raw_line.to_string(),
            reason: "strikethrough '~~' forbidden (delete line when done)".to_string(),
        }));
    }
    if let Some(after_bullet) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        let after = after_bullet.trim_start();
        if after.starts_with("[x]") || after.starts_with("[X]") {
            return Err(DebtLineViolation::CheckedBox(LintError {
                line_number,
                line: raw_line.to_string(),
                reason: "completed checkbox '- [x]' forbidden (delete line when done)".to_string(),
            }));
        }
        if after.starts_with("[ ]") {
            return Err(DebtLineViolation::Schema(LintError {
                line_number,
                line: raw_line.to_string(),
                reason: "checkbox '- [ ]' forbidden (use standard bullet '- ')".to_string(),
            }));
        }
    }
    parse_debt_line(raw_line, line_number).map_err(DebtLineViolation::Schema)
}

fn parse_target_section(section: &str) -> Result<MemorySection> {
    let sec_lower = section.trim().to_lowercase();
    if sec_lower.contains("epic") {
        Ok(MemorySection::Epics)
    } else if sec_lower.contains("debt") {
        Ok(MemorySection::Debt)
    } else if sec_lower.contains("vocab") || sec_lower.contains("glossary") {
        Ok(MemorySection::Vocab)
    } else if sec_lower.contains("gotcha") || sec_lower.contains("trap") {
        Ok(MemorySection::Gotchas)
    } else {
        bail!(
            "Unknown section '{}': expected 'epics', 'debt', 'vocab', or 'gotchas'",
            section
        );
    }
}

fn sanitize_anchor_value<'a>(kind: &str, raw: Option<&'a str>) -> Result<Option<&'a str>> {
    let Some(val) = raw else {
        return Ok(None);
    };
    let trimmed = val.trim();
    if trimmed.is_empty() {
        bail!("Memory entry '{}' anchor must not be empty", kind);
    }
    if trimmed
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '[' | ']'))
    {
        bail!(
            "Memory entry '{}' anchor must not contain newlines or brackets ('[' or ']')",
            kind
        );
    }
    Ok(Some(trimmed))
}

fn format_entry_bullet(
    target_sec: MemorySection,
    clean_text: &str,
    scope: Option<&str>,
    sym: Option<&str>,
) -> Result<String> {
    let clean_scope = sanitize_anchor_value("scope", scope)?;
    let clean_sym = sanitize_anchor_value("sym", sym)?;

    if target_sec == MemorySection::Debt {
        let candidate = if clean_text.starts_with("- ") || clean_text.starts_with("* ") {
            clean_text.to_string()
        } else {
            format!("- {}", clean_text)
        };
        validate_debt_line(&candidate, 1)
            .map_err(|v| anyhow::anyhow!("Invalid debt entry: {}", v.into_error().reason))?;
        return Ok(candidate);
    }

    let mut tags = Vec::new();
    if let Some(sc) = clean_scope {
        tags.push(format!("@scope: {}", sc));
    }
    if let Some(sm) = clean_sym {
        tags.push(format!("@sym: {}", sm));
    }
    let body = clean_text
        .strip_prefix("- ")
        .or_else(|| clean_text.strip_prefix("* "))
        .unwrap_or(clean_text);
    if tags.is_empty() {
        Ok(format!("- {}", body))
    } else {
        Ok(format!("- [{}] {}", tags.join(", "), body))
    }
}

fn insert_bullet_into_content(
    raw_content: &str,
    target_sec: MemorySection,
    bullet: &str,
) -> String {
    let lines: Vec<&str> = raw_content.lines().collect();
    let mut insert_at: Option<usize> = None;
    let mut current: Option<MemorySection> = None;
    let mut fence = FenceState::default();

    let matches_target = |sec: MemorySection| match target_sec {
        MemorySection::Vocab => matches!(sec, MemorySection::Vocab | MemorySection::VocabGotchas),
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
        out.insert(pos, bullet.to_string());
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
        out.push(bullet.to_string());
    }

    let mut updated = out.join("\n");
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated
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
        let _lock = acquire_write_lock(&agents_dir)?;

        if !mem_file.exists() || force {
            Self::atomic_write_unlocked(&mem_file, DEFAULT_MEMORY_TEMPLATE)
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
        let content = read_to_string_bounded(&path, MAX_MEMORY_FILE_BYTES)
            .with_context(|| format!("read memory file {}", path.display()))?;
        Ok(Self::from_str(&content, workspace_root.to_path_buf()))
    }

    /// Execute a transactional read-modify-write closure while holding the OS
    /// advisory lock on `.agents/.memory.lock`. Loads the latest ledger inside
    /// the lock and atomically saves if `ledger.raw_content` changed.
    pub fn with_locked_ledger<T, F>(workspace_root: &Path, f: F) -> Result<T>
    where
        F: FnOnce(&mut MemoryLedger) -> Result<T>,
    {
        if crate::workspace::walker::is_disallowed_workspace_root(workspace_root) {
            bail!(
                "Refusing to mutate memory in home or system directory '{}'",
                workspace_root.display()
            );
        }
        let agents_dir = confine_agents_path(workspace_root, ".agents")?;
        let mem_path = confine_agents_path(workspace_root, ".agents/memory.md")?;
        if !agents_dir.exists() {
            // Surface standard missing-ledger error without creating stray `.agents/`.
            Self::load(workspace_root)?;
        }

        let _lock = acquire_write_lock(&agents_dir)?;
        let mut ledger = Self::load(workspace_root)?;
        let before = ledger.raw_content.clone();
        let res = f(&mut ledger)?;
        if ledger.raw_content != before {
            Self::atomic_write_unlocked(&mem_path, &ledger.raw_content)?;
        }
        Ok(res)
    }

    /// Transactionally scaffold (if missing) and append a structured entry
    /// under an OS advisory file lock.
    pub fn add_entry_locked(
        workspace_root: &Path,
        section: &str,
        text: &str,
        scope: Option<&str>,
        sym: Option<&str>,
    ) -> Result<String> {
        let clean_text = text.trim();
        if clean_text.is_empty() || clean_text.contains('\n') || clean_text.contains('\r') {
            bail!("Memory entry text must be a single non-empty line");
        }
        let target_sec = parse_target_section(section)?;
        let bullet = format_entry_bullet(target_sec, clean_text, scope, sym)?;

        if crate::workspace::walker::is_disallowed_workspace_root(workspace_root) {
            bail!(
                "Refusing to mutate memory in home or system directory '{}'",
                workspace_root.display()
            );
        }
        let agents_dir = confine_agents_path(workspace_root, ".agents")?;
        let mem_path = confine_agents_path(workspace_root, ".agents/memory.md")?;
        fs::create_dir_all(&agents_dir)
            .with_context(|| format!("create dir {}", agents_dir.display()))?;

        let _lock = acquire_write_lock(&agents_dir)?;
        if !mem_path.exists() {
            Self::atomic_write_unlocked(&mem_path, DEFAULT_MEMORY_TEMPLATE)?;
        }
        let mut ledger = Self::load(workspace_root)?;
        let updated = insert_bullet_into_content(&ledger.raw_content, target_sec, &bullet);
        ledger.raw_content = updated;
        Self::atomic_write_unlocked(&mem_path, &ledger.raw_content)?;
        Ok(bullet)
    }

    /// Transactionally resolve matching debt line(s) under an OS advisory file
    /// lock. Returns `(deleted_lines, remaining_debt_count)`.
    pub fn resolve_debt_locked(
        workspace_root: &Path,
        pattern: &str,
        all: bool,
    ) -> Result<(Vec<String>, usize)> {
        Self::with_locked_ledger(workspace_root, |ledger| {
            let deleted = ledger.resolve_in_memory(pattern, all)?;
            let remaining = ledger.raw_debt_lines.len();
            Ok((deleted, remaining))
        })
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
                if let Some(next_section) = classify_memory_heading(line) {
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

    /// Write `self.raw_content` atomically assuming the caller already holds
    /// the `.agents/.memory.lock` OS lock (e.g. inside `with_locked_ledger`).
    pub fn save_unlocked(&self) -> Result<()> {
        let path = confine_agents_path(&self.workspace_root, ".agents/memory.md")?;
        Self::atomic_write_unlocked(&path, &self.raw_content)
    }

    pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
        let write_target = resolve_confined_write_target(path)?;
        let parent = write_target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let _lock = acquire_write_lock(parent)?;
        Self::atomic_write_resolved(&write_target, parent, content)
    }

    pub(crate) fn atomic_write_unlocked(path: &Path, content: &str) -> Result<()> {
        let write_target = resolve_confined_write_target(path)?;
        let parent = write_target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        Self::atomic_write_resolved(&write_target, parent, content)
    }

    fn atomic_write_resolved(write_target: &Path, parent: &Path, content: &str) -> Result<()> {
        let existing_perms = fs::metadata(write_target).ok().map(|m| m.permissions());
        let (tmp_path, mut file) = create_unique_temp_file(write_target)?;

        if let Some(perms) = existing_perms {
            if let Err(err) = file.set_permissions(perms) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err).with_context(|| {
                    format!(
                        "preserve permissions on temporary file {}",
                        tmp_path.display()
                    )
                });
            }
        }

        if let Err(err) = file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(err)
                .with_context(|| format!("write temporary memory file {}", tmp_path.display()));
        }
        drop(file);

        if let Err(err) = fs::rename(&tmp_path, write_target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err)
                .with_context(|| format!("atomic rename memory file {}", write_target.display()));
        }
        sync_parent_dir(parent)?;
        Ok(())
    }

    pub fn lint(&self) -> LintReport {
        let mut schema_errors = Vec::new();
        let mut strikethrough_errors = Vec::new();

        for (line_number, raw_line) in &self.raw_debt_lines {
            if let Err(violation) = validate_debt_line(raw_line, *line_number) {
                match violation {
                    DebtLineViolation::Strikethrough(err) | DebtLineViolation::CheckedBox(err) => {
                        strikethrough_errors.push(*line_number);
                        schema_errors.push(err);
                    }
                    DebtLineViolation::Schema(err) => {
                        schema_errors.push(err);
                    }
                }
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

    fn resolve_in_memory(&mut self, pattern: &str, all: bool) -> Result<Vec<String>> {
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
            if let Some(section) = classify_memory_heading(line) {
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
        Ok(deleted)
    }

    /// Surgically delete matching debt line(s) from `## KNOWN DEBT`.
    /// Acquires the OS advisory lock and reloads the latest on-disk ledger
    /// inside the lock (unless already inside `with_locked_ledger`) to prevent
    /// lost updates under concurrent writers.
    pub fn resolve(&mut self, pattern: &str, all: bool) -> Result<Vec<String>> {
        let agents_dir = confine_agents_path(&self.workspace_root, ".agents")?;
        let mem_path = confine_agents_path(&self.workspace_root, ".agents/memory.md")?;

        if is_lock_held(&agents_dir) {
            let deleted = self.resolve_in_memory(pattern, all)?;
            Self::atomic_write_unlocked(&mem_path, &self.raw_content)?;
            return Ok(deleted);
        }

        fs::create_dir_all(&agents_dir)?;
        let _lock = acquire_write_lock(&agents_dir)?;
        let mut latest = if mem_path.exists() {
            Self::load(&self.workspace_root)?
        } else {
            self.clone()
        };
        let deleted = latest.resolve_in_memory(pattern, all)?;
        Self::atomic_write_unlocked(&mem_path, &latest.raw_content)?;
        *self = latest;
        Ok(deleted)
    }

    fn add_entry_in_memory(&mut self, target_sec: MemorySection, bullet: &str) -> Result<()> {
        let updated = insert_bullet_into_content(&self.raw_content, target_sec, bullet);
        self.raw_content = updated;
        *self = Self::from_str(&self.raw_content, self.workspace_root.clone());
        Ok(())
    }

    /// Append a structured entry to `.agents/memory.md` under `section`
    /// (`epics`, `debt`, `vocab`, `gotchas`), optionally anchored with
    /// `@scope:` or `@sym:` for JIT delivery in `slice`, `blast`, and `diff`.
    /// Transactionally reloads and saves under the OS advisory lock to prevent
    /// lost updates under concurrent writers.
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
        let target_sec = parse_target_section(section)?;
        let bullet = format_entry_bullet(target_sec, clean_text, scope, sym)?;

        let agents_dir = confine_agents_path(&self.workspace_root, ".agents")?;
        let mem_path = confine_agents_path(&self.workspace_root, ".agents/memory.md")?;

        if is_lock_held(&agents_dir) {
            self.add_entry_in_memory(target_sec, &bullet)?;
            Self::atomic_write_unlocked(&mem_path, &self.raw_content)?;
            return Ok(bullet);
        }

        fs::create_dir_all(&agents_dir)?;
        let _lock = acquire_write_lock(&agents_dir)?;
        let mut latest = if mem_path.exists() {
            Self::load(&self.workspace_root)?
        } else {
            self.clone()
        };
        latest.add_entry_in_memory(target_sec, &bullet)?;
        Self::atomic_write_unlocked(&mem_path, &latest.raw_content)?;
        *self = latest;
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

/// Extract the first Markdown H1 heading (`# ...`) outside YAML frontmatter
/// (`---` ... `---`/`...`) and fenced/indented code blocks.
fn extract_markdown_h1(content: &str) -> Option<&str> {
    let mut lines = content.lines().peekable();
    if let Some(&first) = lines.peek() {
        let clean_first = first.strip_prefix('\u{feff}').unwrap_or(first);
        if clean_first.trim() == "---" {
            lines.next();
            for line in lines.by_ref() {
                let t = line.trim();
                if t == "---" || t == "..." {
                    break;
                }
            }
        }
    }

    let mut fence = FenceState::default();
    for line in lines {
        if update_code_fence(line, &mut fence) {
            continue;
        }
        let leading_spaces = line.bytes().take_while(|&b| b == b' ').count();
        if leading_spaces > 3 {
            continue;
        }
        let rest = &line[leading_spaces..];
        if rest.starts_with('\t') {
            continue;
        }
        if let Some(title) = rest.strip_prefix("# ") {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
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
        let Ok(content) = read_to_string_bounded(&confined, MAX_MEMORY_FILE_BYTES) else {
            continue;
        };
        let fname = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if fname.eq_ignore_ascii_case("README.md") {
            continue;
        }
        let title = extract_markdown_h1(&content)
            .unwrap_or(fname.as_str())
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

/// Strip a leading `accepted` or `[accepted]` waiver prefix (case-insensitive,
/// with optional `:` or whitespace boundary). Returns `None` if no valid waiver
/// prefix is present, ensuring `is_accepted_prefix` and normalization never diverge.
pub(crate) fn strip_accepted_prefix(what: &str) -> Option<&str> {
    let w = what.trim();
    if w.len() >= 10 && w.as_bytes()[..10].eq_ignore_ascii_case(b"[accepted]") {
        let rest = &w[10..];
        if rest.is_empty() {
            return Some("");
        }
        if let Some(after_colon) = rest.strip_prefix(':') {
            return Some(after_colon.trim_start());
        }
        let trimmed = rest.trim_start();
        if trimmed.len() < rest.len() {
            return Some(trimmed);
        }
        return None;
    }
    if w.len() >= 8 && w.as_bytes()[..8].eq_ignore_ascii_case(b"accepted") {
        let rest = &w[8..];
        if rest.is_empty() {
            return Some("");
        }
        if let Some(after_colon) = rest.strip_prefix(':') {
            return Some(after_colon.trim_start());
        }
        let trimmed = rest.trim_start();
        if trimmed.len() < rest.len() {
            return Some(trimmed);
        }
        return None;
    }
    None
}

pub(crate) fn is_accepted_prefix(what: &str) -> bool {
    strip_accepted_prefix(what).is_some()
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
    use tempfile::tempdir;

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
        // CommonMark: <=3 leading spaces allowed, >=4 spaces or tab is code block
        assert_eq!(
            classify_memory_heading("   ## KNOWN DEBT"),
            Some(MemorySection::Debt)
        );
        assert_eq!(classify_memory_heading("    ## KNOWN DEBT"), None);
        assert_eq!(classify_memory_heading("\t## KNOWN DEBT"), None);
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
        assert!(!is_accepted_prefix("[accepted]foo"));
        assert!(strip_accepted_prefix("[accepted]foo").is_none());
        assert_eq!(strip_accepted_prefix("[accepted] foo"), Some("foo"));
        assert_eq!(strip_accepted_prefix("accepted: foo"), Some("foo"));
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

    #[test]
    fn add_entry_rejects_anchor_injection_and_invalid_debt() {
        let dir = tempdir().unwrap();
        MemoryLedger::scaffold(dir.path(), false).unwrap();
        let mut ledger = MemoryLedger::load(dir.path()).unwrap();

        assert!(ledger
            .add_entry("gotchas", "ok", Some("src/]\n## KNOWN DEBT"), None)
            .is_err());
        assert!(ledger
            .add_entry("gotchas", "ok", Some("src/[x]"), None)
            .is_err());
        assert!(ledger
            .add_entry("gotchas", "ok", None, Some("foo\rbar"))
            .is_err());
        assert!(ledger
            .add_entry("gotchas", "ok", Some("   "), None)
            .is_err());
        assert!(ledger
            .add_entry("debt", "~~done~~ <- a -> b", None, None)
            .is_err());
        assert!(ledger
            .add_entry("debt", "[x] done <- a -> b", None, None)
            .is_err());
        assert!(ledger
            .add_entry("debt", "[ ] open <- a -> b", None, None)
            .is_err());
    }

    #[test]
    fn adr_title_skips_yaml_frontmatter_and_fences() {
        let md = "\
---
# status: accepted
title: ignored
---

```bash
# shell comment inside fence
```

    # indented code comment

# ADR-001: WAL Concurrency
";
        assert_eq!(extract_markdown_h1(md), Some("ADR-001: WAL Concurrency"));
    }

    #[test]
    fn load_rejects_oversized_memory_file() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agents")).unwrap();
        let big = "a".repeat((MAX_MEMORY_FILE_BYTES + 16) as usize);
        fs::write(dir.path().join(".agents/memory.md"), big).unwrap();
        let err = MemoryLedger::load(dir.path()).unwrap_err();
        assert!(format!("{:#}", err).contains("exceeds maximum allowed size"));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions_and_rejects_symlink_lock() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempdir().unwrap();
        MemoryLedger::scaffold(dir.path(), false).unwrap();
        let mem_path = MemoryLedger::memory_path(dir.path());
        fs::set_permissions(&mem_path, fs::Permissions::from_mode(0o600)).unwrap();

        MemoryLedger::add_entry_locked(dir.path(), "gotchas", "keep perms", None, None).unwrap();
        let mode = fs::metadata(&mem_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode must remain 0600");

        let lock_path = dir.path().join(".agents/.memory.lock");
        symlink("/nonexistent_target_for_test", &lock_path).unwrap();
        let err = MemoryLedger::add_entry_locked(dir.path(), "gotchas", "blocked", None, None)
            .unwrap_err();
        assert!(
            format!("{:#}", err).contains("symlink"),
            "expected symlink rejection, got: {err:#}"
        );
    }

    #[test]
    fn concurrent_add_entry_locked_preserves_all_updates() {
        let dir = tempdir().unwrap();
        MemoryLedger::scaffold(dir.path(), false).unwrap();
        let root = dir.path().to_path_buf();

        let handles: Vec<_> = (0..12)
            .map(|i| {
                let r = root.clone();
                std::thread::spawn(move || {
                    MemoryLedger::add_entry_locked(
                        &r,
                        "gotchas",
                        &format!("concurrent-note-{i}"),
                        None,
                        None,
                    )
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let ledger = MemoryLedger::load(dir.path()).unwrap();
        for i in 0..12 {
            assert!(
                ledger.raw_content.contains(&format!("concurrent-note-{i}")),
                "missing concurrent-note-{i}"
            );
        }
    }
}
