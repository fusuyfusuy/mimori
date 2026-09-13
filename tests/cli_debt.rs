use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_debt_list_finds_polyglot_ponytail_markers() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // 1. Rust file
    fs::write(
        src.join("auth.rs"),
        "// ponytail: bypass cache <- max 100 req/s -> implement redis pool\npub fn auth() {}\n",
    )
    .unwrap();

    // 2. Python file
    fs::write(
        src.join("scraper.py"),
        "# ponytail: unthrottled fetch <- 5 req/s -> add token bucket\ndef scrape(): pass\n",
    )
    .unwrap();

    // 3. CSS/TS block comment
    fs::write(
        src.join("styles.ts"),
        "/* ponytail: inline style <- width 0 -> use ResizeObserver */\nexport const x = 1;\n",
    )
    .unwrap();

    // 4. SQL comment
    fs::write(
        src.join("query.sql"),
        "-- ponytail: full table scan <- rows < 1000 -> create index on user_id\nSELECT 1;\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("debt").arg("list");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(
            "DEBT_SCAN: 4 markers found across 4 files:",
        ))
        .stdout(predicate::str::contains(
            "src/auth.rs:1: bypass cache <- max 100 req/s -> implement redis pool",
        ))
        .stdout(predicate::str::contains(
            "src/scraper.py:1: unthrottled fetch <- 5 req/s -> add token bucket",
        ))
        .stdout(predicate::str::contains(
            "src/styles.ts:1: inline style <- width 0 -> use ResizeObserver",
        ))
        .stdout(predicate::str::contains(
            "src/query.sql:1: full table scan <- rows < 1000 -> create index on user_id",
        ));
}

#[test]
fn test_cli_debt_check_passes_and_fails() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // Valid marker
    let code_path = src.join("app.rs");
    fs::write(
        &code_path,
        "// ponytail: valid marker <- ceiling ok -> trigger ok\npub fn run() {}\n",
    )
    .unwrap();

    let mut cmd_pass = Command::cargo_bin("mimori").unwrap();
    cmd_pass.current_dir(dir.path()).arg("debt").arg("check");
    cmd_pass.assert().success().stdout(predicate::str::contains(
        "DEBT_CHECK: markers: 1; valid_triggers: 1/1; ceiling: 1/30; exit 0.",
    ));

    // Add marker missing trigger
    fs::write(
        src.join("broken.rs"),
        "// ponytail: broken marker <- ceiling only\npub fn broken() {}\n",
    )
    .unwrap();

    let mut cmd_fail = Command::cargo_bin("mimori").unwrap();
    cmd_fail.current_dir(dir.path()).arg("debt").arg("check");
    cmd_fail
        .assert()
        .failure()
        .stdout(predicate::str::contains("DEBT_CHECK_FAIL: markers: 2;"))
        .stdout(predicate::str::contains("valid_triggers: 1/2"))
        .stdout(predicate::str::contains("src/broken.rs:1"));
}

#[test]
fn test_cli_debt_sync_merges_and_cleans_stale() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let initial_memory = "\
# Project Memory

## Active Epics & Scale
- Scale: Initial setup.

## KNOWN DEBT (open only — one line per item, delete when done)
# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>

- accepted test gaps <- daemon loops untested -> deliberate gaps preserved
- stale dead marker <- ceiling -> trigger

## Domain Vocabulary & Gotchas
- Notes: Terms.
";
    fs::write(agents_dir.join("memory.md"), initial_memory).unwrap();

    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("pool.rs"),
        "// ponytail: connection pool bypass <- 20 rps -> add deadpool\npub fn pool() {}\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("debt").arg("sync");
    cmd.assert().success().stdout(predicate::str::contains(
        "DEBT_SYNC: in_code: 1; manual: 1; synced to .agents/memory.md: 2/30 lines; exit 0.",
    ));

    let updated_memory = fs::read_to_string(agents_dir.join("memory.md")).unwrap();
    // Manual accepted item preserved
    assert!(updated_memory
        .contains("accepted test gaps <- daemon loops untested -> deliberate gaps preserved"));
    // New in-code marker added
    assert!(updated_memory.contains("connection pool bypass <- 20 rps -> add deadpool"));
    // Stale dead marker removed
    assert!(!updated_memory.contains("stale dead marker"));
    // Other sections preserved
    assert!(updated_memory.contains("## Active Epics & Scale"));
    assert!(updated_memory.contains("## Domain Vocabulary & Gotchas"));
}

#[test]
fn test_cli_debt_rust_lifetime_and_unicode_handling() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    // Line with single lifetime 'a before comment marker
    fs::write(
        src.join("lifetime.rs"),
        "pub fn parse<'a>(s: &str) { // ponytail: lifetime debt <- cap 1 -> fix lifetime\n}\n",
    )
    .unwrap();

    // Line with non-ASCII / Turkish / German characters
    fs::write(
        src.join("unicode.rs"),
        "// İSTANBUL & GROßE // ponytail: unicode debt <- cap 2 -> test trigger\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("debt").arg("list");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(
            "DEBT_SCAN: 2 markers found across 2 files:",
        ))
        .stdout(predicate::str::contains(
            "lifetime debt <- cap 1 -> fix lifetime",
        ))
        .stdout(predicate::str::contains(
            "unicode debt <- cap 2 -> test trigger",
        ));
}

#[test]
fn test_cli_debt_nested_ignore_directories() {
    let dir = tempdir().unwrap();
    let nested_nm = dir.path().join("packages/core/node_modules");
    let nested_target = dir.path().join("services/api/target");
    let valid_src = dir.path().join("packages/core/src");
    fs::create_dir_all(&nested_nm).unwrap();
    fs::create_dir_all(&nested_target).unwrap();
    fs::create_dir_all(&valid_src).unwrap();

    fs::write(
        nested_nm.join("ignored.js"),
        "// ponytail: nm debt <- cap -> trigger\n",
    )
    .unwrap();
    fs::write(
        nested_target.join("ignored.rs"),
        "// ponytail: target debt <- cap -> trigger\n",
    )
    .unwrap();
    fs::write(
        valid_src.join("core.rs"),
        "// ponytail: valid pkg debt <- cap 5 -> trigger ok\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("debt").arg("list");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains(
            "DEBT_SCAN: 1 markers found across 1 files:",
        ))
        .stdout(predicate::str::contains(
            "packages/core/src/core.rs:1: valid pkg debt",
        ));
}
