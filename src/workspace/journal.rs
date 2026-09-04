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
