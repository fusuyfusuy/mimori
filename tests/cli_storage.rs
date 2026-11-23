use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_sqlite_cache_creation_and_query() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let file1 = src.join("app.rs");
    fs::write(
        &file1,
        "pub fn startup() { init_db(); }\npub fn init_db() {}\n",
    )
    .unwrap();

    // First invocation: builds SQLite cache in .mimori/index.db
    let mut cmd1 = Command::cargo_bin("mimori").unwrap();
    cmd1.current_dir(dir.path()).arg("find").arg("startup");
    cmd1.assert()
        .success()
        .stdout(predicate::str::contains("startup"));

    // Check .mimori/index.db exists
    let db_path = dir.path().join(".mimori").join("index.db");
    assert!(
        db_path.exists(),
        ".mimori/index.db should exist after indexing"
    );

    // Second invocation: instant lookup from SQLite
    let mut cmd2 = Command::cargo_bin("mimori").unwrap();
    cmd2.current_dir(dir.path())
        .arg("slice")
        .arg("src/app.rs:startup");
    cmd2.assert()
        .success()
        .stdout(predicate::str::contains("startup()"))
        .stdout(predicate::str::contains("init_db"));
}

#[test]
fn test_cli_incremental_cache_update_on_edit() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let file = src.join("feature.rs");
    fs::write(&file, "pub fn old_feature() {}\n").unwrap();

    // Index original
    let mut cmd1 = Command::cargo_bin("mimori").unwrap();
    cmd1.current_dir(dir.path()).arg("find").arg("old_feature");
    cmd1.assert()
        .success()
        .stdout(predicate::str::contains("old_feature"));

    // Modify file: rename symbol
    fs::write(&file, "pub fn new_feature() {}\n").unwrap();

    // Re-run: should incrementally re-index and find new_feature
    let mut cmd2 = Command::cargo_bin("mimori").unwrap();
    cmd2.current_dir(dir.path()).arg("find").arg("new_feature");
    cmd2.assert()
        .success()
        .stdout(predicate::str::contains("new_feature"));

    // old_feature should no longer be present
    let mut cmd3 = Command::cargo_bin("mimori").unwrap();
    cmd3.current_dir(dir.path()).arg("find").arg("old_feature");
    cmd3.assert()
        .success()
        .stdout(predicate::str::contains("0 matches"));
}

#[test]
fn test_cli_clean_command_purges_cache() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn test_fn() {}\n").unwrap();

    // Warm up cache
    let mut cmd1 = Command::cargo_bin("mimori").unwrap();
    cmd1.current_dir(dir.path()).arg("find").arg("test_fn");
    cmd1.assert().success();

    let db_path = dir.path().join(".mimori").join("index.db");
    assert!(db_path.exists());

    // Run clean
    let mut cmd_clean = Command::cargo_bin("mimori").unwrap();
    cmd_clean.current_dir(dir.path()).arg("clean");
    cmd_clean.assert().success();

    assert!(
        !db_path.exists(),
        ".mimori/index.db should be purged after clean"
    );
}

#[test]
fn test_cli_init_reports_failure_when_it_creates_nothing() {
    // Regression S1: init printed success and exited 0 even when
    // create_dir_all failed, and it is the first step of the documented
    // warmup, so the failure cascaded silently.
    let dir = tempdir().unwrap();
    // A regular file at .mimori makes create_dir_all fail.
    fs::write(dir.path().join(".mimori"), b"").unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("init");
    cmd.assert().failure();
}

#[test]
fn test_cli_corrupt_sqlite_auto_rebuilds() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("ok.rs"), "pub fn survivor() {}\n").unwrap();

    // Build initial cache, then corrupt index.db with garbage bytes.
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["find", "survivor"])
        .assert()
        .success();

    let db_path = dir.path().join(".mimori/index.db");
    fs::write(&db_path, b"not a valid sqlite database at all!!!").unwrap();

    // Next invocation must auto-rebuild and succeed instead of bricking.
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["find", "survivor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("survivor"));
}

#[cfg(unix)]
#[test]
fn test_cli_readonly_workspace_falls_back_to_memory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("ro.rs"), "pub fn read_only_fn() {}\n").unwrap();

    // Make workspace root read-only so `.mimori/` cannot be created on disk.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let out = Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["find", "read_only_fn"])
        .output()
        .unwrap();

    // Restore permissions before asserting so TempDir cleanup always succeeds.
    let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755));

    assert!(
        out.status.success(),
        "read-only workspace should fall back to in-memory graph; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("read_only_fn"));
}

#[cfg(unix)]
#[test]
fn test_cli_clean_all_refuses_symlinked_mimori_dir() {
    use std::os::unix::fs::symlink;

    let ws = tempdir().unwrap();
    let victim = tempdir().unwrap();
    let victim_cache = victim.path().join(".cache");
    fs::create_dir_all(&victim_cache).unwrap();
    let victim_file = victim_cache.join("precious.txt");
    fs::write(&victim_file, "do not delete").unwrap();

    symlink(victim.path(), ws.path().join(".mimori")).unwrap();

    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(ws.path())
        .args(["clean", "--all"])
        .assert()
        .failure();

    assert!(
        victim_file.exists(),
        "clean --all must never follow symlinked .mimori and delete outside files"
    );
}

#[test]
fn test_cli_concurrent_sync_never_returns_zero_symbols() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..8 {
        fs::write(
            src.join(format!("m{i}.rs")),
            format!("pub fn f_{i}() {{ g_{i}(); }}\npub fn g_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let root = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let out = Command::cargo_bin("mimori")
                    .unwrap()
                    .current_dir(&root)
                    .args(["--json", "doctor"])
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "concurrent doctor failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(parsed["symbols"].as_u64().unwrap(), 16);
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
