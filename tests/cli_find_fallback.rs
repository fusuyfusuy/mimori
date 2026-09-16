use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_find_fallback_to_literal_matches() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let service_code = r#"export function scheduleTask(jobType: string) {
    if (jobType === "create-backup") {
        triggerBackupJob();
    }
}
"#;

    fs::write(root.join("service.ts"), service_code).unwrap();

    // Query "create-backup" is not a symbol name or a file name, so AST match yields 0.
    // It should fall back to matching the literal string inside scheduleTask.
    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(root)
        .args(["find", "create-backup"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("literal matches")
                .or(predicate::str::contains("literal match")),
        )
        .stdout(predicate::str::contains("service.ts"))
        .stdout(predicate::str::contains(
            "if (jobType === \"create-backup\")",
        ));
}

#[test]
fn test_cli_find_fallback_in_file_without_symbols_and_mjs() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // script.mjs has no function/class/struct/type declarations (zero AST symbols)
    let script_code = r#"// Pure top-level script
console.log("SECRET_API_LITERAL_TOKEN");
"#;

    fs::write(root.join("script.mjs"), script_code).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(root)
        .args(["find", "SECRET_API_LITERAL_TOKEN"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("literal matches")
                .or(predicate::str::contains("literal match")),
        )
        .stdout(predicate::str::contains("script.mjs"))
        .stdout(predicate::str::contains("SECRET_API_LITERAL_TOKEN"));
}

#[test]
fn test_cli_find_and_slice_cjs_symbols() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let cjs_code = r#"function helperUtility(x) {
    return x * 2;
}
module.exports = { helperUtility };
"#;

    fs::write(root.join("utils.cjs"), cjs_code).unwrap();

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(root)
        .args(["find", "helperUtility"])
        .assert()
        .success()
        .stdout(predicate::str::contains("helperUtility"))
        .stdout(predicate::str::contains("utils.cjs"));

    let mut slice_cmd = Command::cargo_bin("mimori").unwrap();
    slice_cmd
        .current_dir(root)
        .args(["slice", "utils.cjs:helperUtility"])
        .assert()
        .success()
        .stdout(predicate::str::contains("function helperUtility(x)"));
}
