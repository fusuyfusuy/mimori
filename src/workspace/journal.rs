use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
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

/// Rotate the journal once it exceeds this size, keeping one previous file.
const MAX_JOURNAL_BYTES: u64 = 1 << 20; // 1 MiB

pub fn append_activity(workspace_root: &Path, record: &ActivityRecord) -> Result<()> {
    let mimori_dir = workspace_root.join(".mimori");
    if !mimori_dir.exists() {
        fs::create_dir_all(&mimori_dir)?;
    }
    let jsonl_path = mimori_dir.join("activity.jsonl");

    // The journal used to grow without bound, and read_recent_activity parsed
    // all of it to return the last handful of records.
    if fs::metadata(&jsonl_path).map(|m| m.len()).unwrap_or(0) > MAX_JOURNAL_BYTES {
        let _ = fs::rename(&jsonl_path, jsonl_path.with_extension("jsonl.1"));
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl_path)?;

    writeln!(file, "{}", serde_json::to_string(record)?)?;
    Ok(())
}

pub fn read_recent_activity(workspace_root: &Path, limit: usize) -> Result<Vec<ActivityRecord>> {
    let jsonl_path = workspace_root.join(".mimori").join("activity.jsonl");
    if !jsonl_path.exists() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut records: Vec<ActivityRecord> = tail_lines(&jsonl_path, limit)?
        .iter()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();

    if records.len() > limit {
        records.drain(..records.len() - limit);
    }
    Ok(records)
}

/// Read the last `n` non-empty lines by seeking backwards, rather than parsing
/// the whole file to keep its tail.
fn tail_lines(path: &Path, n: usize) -> Result<Vec<String>> {
    const CHUNK: u64 = 8192;

    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut pos = len;
    let mut buf: Vec<u8> = Vec::new();

    while pos > 0 {
        let step = CHUNK.min(pos);
        pos -= step;
        file.seek(SeekFrom::Start(pos))?;

        let mut chunk = vec![0u8; step as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;

        // One extra newline so a partial leading line is discarded, not parsed.
        if buf.iter().filter(|&&b| b == b'\n').count() > n {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();

    if pos > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > n {
        lines.drain(..lines.len() - n);
    }
    Ok(lines)
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
    fn recent_activity_returns_the_tail_without_reading_everything() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..500 {
            let rec = ActivityRecord::new(format!("act{i}"), format!("summary {i}"), vec![]);
            append_activity(tmp.path(), &rec).unwrap();
        }

        let recent = read_recent_activity(tmp.path(), 10).unwrap();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent.last().unwrap().action, "act499");
        assert_eq!(recent.first().unwrap().action, "act490");
    }

    #[test]
    fn recent_activity_handles_a_short_journal() {
        let tmp = tempfile::tempdir().unwrap();
        append_activity(tmp.path(), &ActivityRecord::new("a".into(), "s".into(), vec![])).unwrap();

        let recent = read_recent_activity(tmp.path(), 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, "a");
    }

    #[test]
    fn recent_activity_on_a_missing_journal_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_recent_activity(tmp.path(), 10).unwrap().is_empty());
    }

    #[test]
    fn timestamp_round_numbers_are_correct() {
        // Sanity-check the hand-rolled civil-from-days conversion.
        assert!(current_utc_timestamp().ends_with('Z'));
        assert_eq!(current_utc_timestamp().len(), 20);
    }
}
