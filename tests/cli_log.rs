use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_log_creates_activity_jsonl() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // Create a dummy file so workspace has content
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(root)
        .arg("log")
        .arg("--action")
        .arg("feat-auth")
        .arg("--summary")
        .arg("Added JWT authentication middleware")
        .arg("--files")
        .arg("src/auth.rs,src/middleware.rs");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Logged action: feat-auth"));

    let jsonl_path = root.join(".mimori").join("activity.jsonl");
    assert!(jsonl_path.exists());

    let content = fs::read_to_string(&jsonl_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);

    let parsed: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["action"], "feat-auth");
    assert_eq!(parsed["summary"], "Added JWT authentication middleware");
    assert_eq!(
        parsed["files"],
        serde_json::json!(["src/auth.rs", "src/middleware.rs"])
    );
    assert!(parsed["timestamp"].is_string());
}

#[test]
fn test_cli_log_json_output() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(root)
        .arg("--json")
        .arg("log")
        .arg("-a")
        .arg("db-migration")
        .arg("-s")
        .arg("Added users table migration");

    let assert = cmd.assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["action"], "db-migration");
    assert_eq!(parsed["summary"], "Added users table migration");
    assert_eq!(parsed["files"], serde_json::json!([]));
}

#[test]
fn test_cli_dump_includes_recent_activity() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("lib.rs"),
        "pub fn calculate() -> i32 { 42 }\n",
    )
    .unwrap();

    // Log two actions
    let mut log_cmd1 = Command::cargo_bin("mimori").unwrap();
    log_cmd1
        .current_dir(root)
        .args(["log", "-a", "act-one", "-s", "First action", "-f", "lib.rs"])
        .assert()
        .success();

    let mut log_cmd2 = Command::cargo_bin("mimori").unwrap();
    log_cmd2
        .current_dir(root)
        .args(["log", "-a", "act-two", "-s", "Second action"])
        .assert()
        .success();

    // Verify dump in Markdown mode
    let mut dump_cmd = Command::cargo_bin("mimori").unwrap();
    dump_cmd
        .current_dir(root)
        .arg("dump")
        .assert()
        .success()
        .stdout(predicate::str::contains("## 📜 Recent Activity"))
        .stdout(predicate::str::contains("`act-one`"))
        .stdout(predicate::str::contains("First action"))
        .stdout(predicate::str::contains("`act-two`"))
        .stdout(predicate::str::contains("Second action"));

    // Verify dump in JSON mode
    let mut dump_json_cmd = Command::cargo_bin("mimori").unwrap();
    let assert = dump_json_cmd
        .current_dir(root)
        .args(["--json", "dump"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let recent = parsed["recent_activity"].as_array().expect("recent_activity array");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0]["action"], "act-one");
    assert_eq!(recent[1]["action"], "act-two");
}
