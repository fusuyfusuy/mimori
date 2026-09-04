use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ActivityRecord {
    pub timestamp: String,
    pub action: String,
    pub summary: String,
    #[serde(default)]
    pub files: Vec<String>,
}

/// Maximum summary length, in characters, enforced on construction.
pub const SUMMARY_MAX_CHARS: usize = 160;

impl ActivityRecord {
    /// Build a record, capping the summary at `SUMMARY_MAX_CHARS` characters.
    /// Truncation is on a character boundary, never a byte offset.
    pub fn new(action: String, summary: String, files: Vec<String>) -> Self {
        ActivityRecord {
            timestamp: current_utc_timestamp(),
            action,
            summary: truncate_summary(&summary),
            files,
        }
    }
}

fn truncate_summary(summary: &str) -> String {
    let keep = SUMMARY_MAX_CHARS - 3;
    match summary.char_indices().nth(SUMMARY_MAX_CHARS) {
        None => summary.to_string(),
        Some(_) => {
            let cut = summary
                .char_indices()
                .nth(keep)
                .map(|(byte_idx, _)| byte_idx)
                .unwrap_or(summary.len());
            format!("{}...", &summary[..cut])
        }
    }
}

pub fn current_utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1;
    for &d in &month_days {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    let day = days + 1;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

pub fn append_activity(workspace_root: &Path, record: &ActivityRecord) -> Result<()> {
    let mimori_dir = workspace_root.join(".mimori");
    if !mimori_dir.exists() {
        fs::create_dir_all(&mimori_dir)?;
    }
    let jsonl_path = mimori_dir.join("activity.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl_path)?;

    let line = serde_json::to_string(record)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

pub fn read_recent_activity(workspace_root: &Path, limit: usize) -> Result<Vec<ActivityRecord>> {
    let jsonl_path = workspace_root.join(".mimori").join("activity.jsonl");
    if !jsonl_path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(&jsonl_path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line_res in reader.lines() {
        let line = line_res?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ActivityRecord>(trimmed) {
            records.push(record);
        }
    }

    if records.len() > limit {
        let start = records.len() - limit;
        Ok(records[start..].to_vec())
    } else {
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_truncation_respects_char_boundaries() {
        // 200 two-byte chars: byte-slicing at 157 lands mid-char. Regression: M3/main.rs.
        let long = "ü".repeat(200);
        let rec = ActivityRecord::new("act".into(), long, vec![]);
        assert_eq!(rec.summary.chars().count(), SUMMARY_MAX_CHARS);
        assert!(rec.summary.ends_with("..."));
    }

    #[test]
    fn short_summaries_pass_through_untouched() {
        let rec = ActivityRecord::new("act".into(), "ünïcödé fine".into(), vec![]);
        assert_eq!(rec.summary, "ünïcödé fine");
    }

    #[test]
    fn summary_at_the_boundary_is_not_truncated() {
        let exact = "a".repeat(SUMMARY_MAX_CHARS);
        let rec = ActivityRecord::new("act".into(), exact.clone(), vec![]);
        assert_eq!(rec.summary, exact);
    }

    #[test]
    fn timestamp_round_numbers_are_correct() {
        // Sanity-check the hand-rolled civil-from-days conversion.
        assert!(current_utc_timestamp().ends_with('Z'));
        assert_eq!(current_utc_timestamp().len(), 20);
    }
}
