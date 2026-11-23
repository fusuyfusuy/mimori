use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn fixture() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.rs"),
        "pub fn used() {}\npub fn caller() { used(); }\npub fn lonely() {}\n",
    )
    .unwrap();
    fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    dir
}

#[test]
fn test_cli_doctor_reports_health_and_dead_weight() {
    let dir = fixture();
    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("doctor");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Doctor"))
        .stdout(predicate::str::contains("lonely"))
        .stdout(predicate::str::contains("Symbols:"));
}

#[test]
fn test_cli_doctor_json_is_structured() {
    let dir = fixture();
    let out = Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let val: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(val["files"].as_u64(), Some(2));
    assert!(val["symbols"].as_u64().unwrap() >= 4);
    assert!(val["likely_dead"].is_array());
    assert!(val["needs_review"].is_array());
    assert!(val["top_hubs"].is_array());
    assert!(val["resolved"].is_u64());
    assert!(val["pagerank_converged"].is_boolean());
}

#[test]
fn test_cli_doctor_excludes_cfg_test_from_dead_and_hubs() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        r#"
pub fn core_api() { helper(); }
fn helper() {}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_fixture_helper() {}
    #[test]
    fn test_one() { test_fixture_helper(); core_api(); }
    #[test]
    fn test_two() { test_fixture_helper(); }
    #[test]
    fn test_three() { test_fixture_helper(); }
}
"#,
    )
    .unwrap();

    let out = Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let dead_names: Vec<&str> = val["likely_dead"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["name"].as_str())
        .collect();
    assert!(
        !dead_names.contains(&"test_fixture_helper") && !dead_names.contains(&"test_one"),
        "cfg(test) symbols must not be flagged as dead: {:?}",
        dead_names
    );

    let hub_names: Vec<&str> = val["top_hubs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["name"].as_str())
        .collect();
    assert!(
        !hub_names.contains(&"test_fixture_helper"),
        "cfg(test) helpers must not pollute top_hubs: {:?}",
        hub_names
    );
}

#[test]
fn test_cli_blast_down_follows_callees() {
    let dir = fixture();
    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("blast")
        .arg("src/a.rs:caller")
        .arg("--down");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Downstream"))
        .stdout(predicate::str::contains("used"));
}
