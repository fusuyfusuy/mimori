use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_dump_combines_map_gotchas_debt() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let memory_content = "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
- cache bypass <- max 100 rps -> add redis

## Domain Vocabulary & Gotchas
- Turkish i: normalize_text handles dotted vs dotless i.
";
    fs::write(agents_dir.join("memory.md"), memory_content).unwrap();

    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.rs"),
        "pub fn startup() { run_server(); }\npub fn run_server() {}\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("dump")
        .arg("--budget")
        .arg("1500");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(
            "# MIMORI TURN-0 CONTEXT (tokens: ~",
        ))
        .stdout(predicate::str::contains("/ budget: 1500)"))
        .stdout(predicate::str::contains("## DOMAIN VOCABULARY & GOTCHAS"))
        .stdout(predicate::str::contains(
            "Turkish i: normalize_text handles dotted vs dotless i.",
        ))
        .stdout(predicate::str::contains("## ACTIVE DEBT (1/30)"))
        .stdout(predicate::str::contains(
            "- cache bypass <- max 100 rps -> add redis",
        ))
        .stdout(predicate::str::contains(
            "## ARCHITECTURAL MAP (PageRank Centrality)",
        ))
        .stdout(predicate::str::contains("startup"));
}

#[test]
fn test_cli_dump_respects_budget() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let mut memory = String::from("# Project Memory\n\n## Domain Vocabulary & Gotchas\n");
    for i in 0..50 {
        memory.push_str(&format!(
            "- Gotcha {}: Long explanation of edge cases in production.\n",
            i
        ));
    }
    fs::write(agents_dir.join("memory.md"), memory).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("dump")
        .arg("--budget")
        .arg("80");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains("# MIMORI TURN-0 CONTEXT"));
    assert!(stdout.contains("truncated to fit token budget"));
}

#[test]
fn test_cli_dump_json_output() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    fs::write(
        agents_dir.join("memory.md"),
        "# Project Memory\n\n## KNOWN DEBT (open only — one line per item, delete when done)\n- bypass cache <- 100 rps -> add redis\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("--json").arg("dump");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("\"budget\": 1500"))
        .stdout(predicate::str::contains("\"debt_count\": 1"));
}
