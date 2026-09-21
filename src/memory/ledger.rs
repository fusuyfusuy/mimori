use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_DEBT_CEILING: usize = 30;

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
            format!(
                "MEM_LINT: debt: {}/{} lines; schema: valid; no_strikethrough: ok; exit 0.",
                self.debt_count, self.max_ceiling
            )
        } else {
            let mut parts = Vec::new();
            if self.ceiling_breached {
                parts.push(format!(
                    "ceiling breached: {}/{} lines",
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
    pub vocab_gotchas: String,
}

impl MemoryLedger {
    pub fn memory_path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".agents").join("memory.md")
    }

    pub fn decisions_path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".agents").join("decisions.md")
    }

    pub fn scaffold(workspace_root: &Path, force: bool) -> Result<(bool, bool)> {
        let agents_dir = workspace_root.join(".agents");
        fs::create_dir_all(&agents_dir)
            .with_context(|| format!("create dir {}", agents_dir.display()))?;

        let mem_file = Self::memory_path(workspace_root);
        let memory_created = if !mem_file.exists() || force {
            let template = "\
# Project Memory

## Active Epics & Scale
- Scale: Baseline architecture initialized.

## KNOWN DEBT (open only — one line per item, delete when done)
# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>

## Domain Vocabulary & Gotchas
- Terms: Define key domain vocabulary here.
";
            fs::write(&mem_file, template)
                .with_context(|| format!("write {}", mem_file.display()))?;
            true
        } else {
            false
        };

        let dec_file = Self::decisions_path(workspace_root);
        let decisions_created = if !dec_file.exists() || force {
            let template = "\
# Architectural Decisions (ADRs)

## ADR-0001: Architecture Baseline
- **Status**: Accepted
- **Context**: Project initialized with mimori substrate.
- **Decision**: Adopt M2M language contract and .mimori/ cache vs .agents/ memory boundaries.
- **Consequences**: Deterministic code intelligence and verifiable technical debt tracking.
";
            fs::write(&dec_file, template)
                .with_context(|| format!("write {}", dec_file.display()))?;
            true
        } else {
            false
        };

        Ok((memory_created, decisions_created))
    }

    pub fn load(workspace_root: &Path) -> Result<Self> {
        let path = Self::memory_path(workspace_root);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read memory file {}", path.display()))?;
        Ok(Self::from_str(&content, workspace_root.to_path_buf()))
    }

    pub fn from_str(content: &str, workspace_root: PathBuf) -> Self {
        let lines: Vec<&str> = content.lines().collect();
        let mut epics = String::new();
        let mut vocab_gotchas = String::new();
        let mut debt_items = Vec::new();
        let mut raw_debt_lines = Vec::new();

        let mut current_section = "";
        let mut section_content = String::new();

        for (idx, &line) in lines.iter().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with("## ") {
                // Save previous section
                if current_section.eq_ignore_ascii_case("epics") {
                    epics = section_content.trim().to_string();
                } else if current_section.eq_ignore_ascii_case("vocab")
                    || current_section.eq_ignore_ascii_case("gotchas")
                {
                    if vocab_gotchas.is_empty() {
                        vocab_gotchas = section_content.trim().to_string();
                    } else {
                        vocab_gotchas.push_str("\n\n");
                        vocab_gotchas.push_str(section_content.trim());
                    }
                }
                section_content.clear();

                let heading = trimmed.trim_start_matches('#').trim().to_lowercase();
                if heading.contains("epic") {
                    current_section = "epics";
                } else if heading.contains("known debt") || heading.contains("debt") {
                    current_section = "debt";
                } else if heading.contains("vocab") || heading.contains("gotcha") {
                    current_section = "vocab";
                } else {
                    current_section = "other";
                }
                continue;
            }

            if current_section == "debt" {
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    raw_debt_lines.push((line_number, line.to_string()));
                    if let Ok(item) = parse_debt_line(line, line_number) {
                        debt_items.push(item);
                    }
                }
            } else if !current_section.is_empty() {
                section_content.push_str(line);
                section_content.push('\n');
            }
        }

        // Final section flush
        if current_section.eq_ignore_ascii_case("epics") {
            epics = section_content.trim().to_string();
        } else if current_section.eq_ignore_ascii_case("vocab")
            || current_section.eq_ignore_ascii_case("gotchas")
        {
            if vocab_gotchas.is_empty() {
                vocab_gotchas = section_content.trim().to_string();
            } else {
                vocab_gotchas.push_str("\n\n");
                vocab_gotchas.push_str(section_content.trim());
            }
        }

        Self {
            workspace_root,
            raw_content: content.to_string(),
            epics,
            debt_items,
            raw_debt_lines,
            vocab_gotchas,
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::memory_path(&self.workspace_root);
        Self::atomic_write(&path, &self.raw_content)
    }

    pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&tmp_path, content)
            .with_context(|| format!("write temporary memory file {}", tmp_path.display()))?;
        fs::rename(&tmp_path, path)
            .with_context(|| format!("atomic rename memory file {}", path.display()))?;
        Ok(())
    }

    pub fn lint(&self) -> LintReport {
        let mut schema_errors = Vec::new();
        let mut strikethrough_errors = Vec::new();

        for (line_number, raw_line) in &self.raw_debt_lines {
            if raw_line.contains("~~") {
                strikethrough_errors.push(*line_number);
                schema_errors.push(LintError {
                    line_number: *line_number,
                    line: raw_line.clone(),
                    reason: "strikethrough '~~' forbidden (delete line when done)".to_string(),
                });
                continue;
            }
            if raw_line.contains("[x]") || raw_line.contains("[X]") {
                strikethrough_errors.push(*line_number);
                schema_errors.push(LintError {
                    line_number: *line_number,
                    line: raw_line.clone(),
                    reason: "completed checkbox '- [x]' forbidden (delete line when done)"
                        .to_string(),
                });
                continue;
            }
            if raw_line.contains("[ ]") {
                schema_errors.push(LintError {
                    line_number: *line_number,
                    line: raw_line.clone(),
                    reason: "checkbox '- [ ]' forbidden (use standard bullet '- ')".to_string(),
                });
                continue;
            }

            match parse_debt_line(raw_line, *line_number) {
                Ok(_) => {}
                Err(err) => {
                    schema_errors.push(err);
                }
            }
        }

        let debt_count = self.raw_debt_lines.len();
        let ceiling_breached = debt_count > MAX_DEBT_CEILING;
        let passed =
            !ceiling_breached && schema_errors.is_empty() && strikethrough_errors.is_empty();

        LintReport {
            debt_count,
            max_ceiling: MAX_DEBT_CEILING,
            schema_errors,
            strikethrough_errors,
            ceiling_breached,
            passed,
        }
    }

    pub fn resolve(&mut self, pattern: &str) -> Result<usize> {
        let lines: Vec<&str> = self.raw_content.lines().collect();
        let mut new_lines = Vec::with_capacity(lines.len());
        let mut in_debt_section = false;
        let mut deleted_count = 0;

        let pattern_lower = pattern.to_lowercase();

        for line in lines {
            let trimmed = line.trim();
            if trimmed.starts_with("## ") {
                let heading = trimmed.trim_start_matches('#').trim().to_lowercase();
                in_debt_section = heading.contains("known debt") || heading.contains("debt");
                new_lines.push(line);
                continue;
            }

            if in_debt_section
                && (trimmed.starts_with("- ") || trimmed.starts_with("* "))
                && line.to_lowercase().contains(&pattern_lower)
            {
                deleted_count += 1;
                continue; // drop this line
            }

            new_lines.push(line);
        }

        if deleted_count > 0 {
            let mut updated = new_lines.join("\n");
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            self.raw_content = updated;
            *self = Self::from_str(&self.raw_content, self.workspace_root.clone());
            self.save()?;
        }

        Ok(deleted_count)
    }

    pub fn get_section(&self, section: &str) -> Option<String> {
        let sec = section.to_lowercase();
        if sec.contains("epic") {
            if self.epics.is_empty() {
                None
            } else {
                Some(self.epics.clone())
            }
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
        } else if sec.contains("vocab") || sec.contains("gotcha") {
            if self.vocab_gotchas.is_empty() {
                None
            } else {
                Some(self.vocab_gotchas.clone())
            }
        } else {
            None
        }
    }
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

    let is_accepted = what.to_lowercase().starts_with("accepted")
        || what.to_lowercase().starts_with("[accepted]");

    Ok(DebtItem {
        what: what.to_string(),
        why: why.to_string(),
        trigger: trigger.to_string(),
        raw: raw.to_string(),
        line_number,
        is_accepted,
    })
}
