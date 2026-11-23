use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_init_scaffolds_agents_and_mimori() {
    let dir = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("init");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("INIT: .mimori cache ready"))
        .stdout(predicate::str::contains(".gitignore created"))
        .stdout(predicate::str::contains(".agents/ initialized"));

    assert!(dir.path().join(".mimori").is_dir());
    assert!(dir.path().join(".agents").join("memory.md").is_file());
    assert!(!dir.path().join(".agents").join("decisions.md").exists());

    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".mimori/"));

    // Second init invocation: idempotent, preserves existing files
    let mut cmd2 = Command::cargo_bin("mimori").unwrap();
    cmd2.current_dir(dir.path()).arg("init");
    cmd2.assert()
        .success()
        .stdout(predicate::str::contains(".gitignore already configured"))
        .stdout(predicate::str::contains(".agents/ verified"));
}

#[test]
fn test_cli_memory_lint_pass() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## Active Epics & Scale
- Scale: 1000 nodes.

## KNOWN DEBT (open only — one line per item, delete when done)
- cache bypass <- max 100 rps -> implement redis pool
- accepted test gaps <- daemon untested -> deliberate gaps preserved

## Domain Vocabulary & Gotchas
- Gotchas: Turkish i casing.
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert().success().stdout(predicate::str::contains(
        "MEM_LINT: debt: 2/30 lines; schema: valid; no_strikethrough: ok; exit 0.",
    ));
}

#[test]
fn test_cli_memory_lint_ceiling_breach() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let mut content = String::from(
        "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
",
    );
    for i in 1..=32 {
        content.push_str(&format!(
            "- debt item {} <- reason {} -> trigger {}\n",
            i, i, i
        ));
    }

    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert().success().stdout(predicate::str::contains(
        "MEM_LINT_WARN: debt: 32/30 lines (soft ceiling exceeded); schema: valid; no_strikethrough: ok; exit 0.",
    ));
}

#[test]
fn test_cli_memory_lint_schema_fail() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
- missing trigger <- reason only
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("MEM_LINT_FAIL:"))
        .stdout(predicate::str::contains(
            "line 4 malformed: missing '->' trigger separator",
        ));
}

#[test]
fn test_cli_memory_lint_strikethrough_fail() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
- ~~done item~~ <- reason -> trigger
- [x] completed checkbox <- reason -> trigger
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("MEM_LINT_FAIL:"))
        .stdout(predicate::str::contains("strikethrough '~~' forbidden"))
        .stdout(predicate::str::contains(
            "completed checkbox '- [x]' forbidden",
        ));
}

#[test]
fn test_cli_memory_resolve() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## Active Epics & Scale
- Scale: 50 nodes.

## KNOWN DEBT (open only — one line per item, delete when done)
- redis pool migration <- 50 conn ceiling -> add deadpool-redis
- unbatched query <- count <= 10 -> bulk insert API

## Domain Vocabulary & Gotchas
- Gotchas: Keep memory compact.
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("memory")
        .arg("resolve")
        .arg("redis pool migration");
    cmd.assert().success().stdout(predicate::str::contains(
        "MEM_RESOLVE: deleted 1 lines matching 'redis pool migration'; debt: 1/30 lines; exit 0.",
    ));

    let updated = fs::read_to_string(agents_dir.join("memory.md")).unwrap();
    assert!(!updated.contains("redis pool migration"));
    assert!(updated.contains("unbatched query"));
    assert!(updated.contains("Active Epics & Scale"));
}

#[test]
fn test_cli_memory_show() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## Active Epics & Scale
- Scale: 120 sources.

## KNOWN DEBT (open only — one line per item, delete when done)
- asyncpg pool <- 20 max -> tune pgbouncer

## Domain Vocabulary & Gotchas
- Currency: TRY formatting 1.234,56.
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    // Show section vocab
    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("memory")
        .arg("show")
        .arg("--section")
        .arg("vocab");
    cmd.assert().success().stdout(predicate::str::contains(
        "Currency: TRY formatting 1.234,56.",
    ));

    // JSON output
    let mut cmd2 = Command::cargo_bin("mimori").unwrap();
    cmd2.current_dir(dir.path()).arg("--json").arg("memory");
    cmd2.assert()
        .success()
        .stdout(predicate::str::contains("\"debt_count\": 1"));
}

#[test]
fn test_cli_memory_lint_missing_bullet_prefix_fail() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
forgot bullet prefix <- reason -> trigger
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("MEM_LINT_FAIL:"))
        .stdout(predicate::str::contains("missing bullet prefix '- '"));
}

#[test]
fn test_cli_memory_lint_asterisk_checkbox_fail() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let content = "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
* [x] completed with asterisk <- reason -> trigger
";
    fs::write(agents_dir.join("memory.md"), content).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("memory").arg("lint");
    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("MEM_LINT_FAIL:"))
        .stdout(predicate::str::contains(
            "completed checkbox '- [x]' forbidden",
        ));
}

#[test]
fn test_cli_memory_add_and_safe_resolve() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();
    fs::write(
        agents_dir.join("memory.md"),
        "\
# Project Memory

## KNOWN DEBT (open only — one line per item, delete when done)
- redis pool one <- 10 max -> shard
- redis pool two <- 20 max -> shard

## Domain Vocabulary & Gotchas
",
    )
    .unwrap();

    // Add scope+sym anchored gotcha
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args([
            "memory",
            "add",
            "--section",
            "gotchas",
            "--scope",
            "src/db/",
            "--sym",
            "connect",
            "Keep transactions atomic.",
        ])
        .assert()
        .success();

    let updated = fs::read_to_string(agents_dir.join("memory.md")).unwrap();
    assert!(updated.contains("@scope: src/db/"));
    assert!(updated.contains("@sym: connect"));
    assert!(updated.contains("Keep transactions atomic."));

    // Reject overly short/punctuation-only patterns like "-"
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["memory", "resolve", "-"])
        .assert()
        .failure();

    // Reject multi-match without --all
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["memory", "resolve", "redis pool"])
        .assert()
        .failure();

    // Both lines still intact
    let intact = fs::read_to_string(agents_dir.join("memory.md")).unwrap();
    assert!(intact.contains("redis pool one"));
    assert!(intact.contains("redis pool two"));

    // Succeed with --all
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["memory", "resolve", "--all", "redis pool"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted 2 lines"));
}

#[test]
fn test_cli_debt_sync_preserves_over_30_waivers_and_h1_sections() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let mut content = String::from(
        "# Project Memory\n\n## KNOWN DEBT (open only — one line per item, delete when done)\n",
    );
    for i in 0..32 {
        content.push_str(&format!("- accepted waiver-{i:02} <- reason -> trigger\n"));
    }
    content.push_str("\n# Appendix\n- Keep this H1 appendix verbatim.\n");
    fs::write(agents_dir.join("memory.md"), &content).unwrap();

    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["debt", "sync"])
        .assert()
        .success();

    let synced = fs::read_to_string(agents_dir.join("memory.md")).unwrap();
    assert!(
        synced.contains("waiver-31"),
        "waiver #32 must never be silently truncated"
    );
    assert!(
        synced.contains("# Appendix"),
        "trailing H1 section must be preserved"
    );
    assert!(synced.contains("Keep this H1 appendix verbatim."));
}
