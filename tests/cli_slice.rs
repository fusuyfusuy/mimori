use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_cli_slice_rust_function() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("calculator.rs");
    let code = r#"
/// Adds two numbers together
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}
"#;
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:add", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains(
            "pub fn add(a: i32, b: i32) -> i32",
        ))
        .stdout(predicate::str::contains("a + b"))
        .stdout(predicate::str::contains("calculator.rs"));
}

#[test]
fn test_cli_slice_rust_struct_and_impl() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("model.rs");
    let code = r#"
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn distance(&self, other: &Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}
"#;
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:Point", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("struct Point"))
        .stdout(predicate::str::contains("pub x: f64"));

    // Slice the method
    let target_method = format!("{}:distance", file_path.to_str().unwrap());
    let mut cmd2 = Command::cargo_bin("mimori").unwrap();
    cmd2.current_dir(dir.path())
        .arg("slice")
        .arg(&target_method);
    cmd2.assert()
        .success()
        .stdout(predicate::str::contains("pub fn distance"));
}

#[test]
fn test_cli_slice_typescript_class_and_method() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("service.ts");
    let code = r#"
export interface User {
    id: string;
    name: string;
}

export class UserService {
    private users: User[] = [];

    public findUser(id: string): User | undefined {
        return this.users.find(u => u.id === id);
    }
}
"#;
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:findUser", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("findUser"))
        .stdout(predicate::str::contains(
            "public findUser(id: string): User | undefined",
        ));
}

#[test]
fn test_cli_slice_line_range() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("sample.rs");
    let code = "line 1\nline 2\nline 3\nline 4\nline 5\n";
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:#L2-4", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("line 2"))
        .stdout(predicate::str::contains("line 3"))
        .stdout(predicate::str::contains("line 4"));
}

#[test]
fn test_cli_slice_json_output() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("sample.ts");
    let code = "export function greet(name: string): string { return `Hello ${name}`; }";
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:greet", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("slice")
        .arg(&target)
        .arg("--json");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("Valid JSON output");
    assert_eq!(json["symbol"]["name"], "greet");
    assert!(json["content"].as_str().unwrap().contains("Hello ${name}"));
}

#[test]
fn test_cli_slice_large_symbol_truncation() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.rs");
    let mut code = String::from("pub fn large_function() {\n");
    for i in 1..=300 {
        code.push_str(&format!("    let x_{} = {};\n", i, i));
    }
    code.push_str("}\n");
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:large_function", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert().success().stdout(predicate::str::contains(
        "lines truncated for token efficiency",
    ));
}

#[test]
fn test_cli_slice_symbol_not_found_error() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("simple.rs");
    let code = "pub fn foo() {}\n";
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:bar", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("not found in workspace"));
}

#[test]
fn test_cli_slice_budget_truncates_context_keeps_core() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("svc.rs");
    let code = "use crate::db;\nuse crate::net;\npub fn handle() { db::save(); net::send(); }\npub fn caller_one() { handle(); }\npub fn caller_two() { handle(); }\n";
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:handle", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    // Budget 0 deterministically exceeds: every droppable section goes, the
    // core stays, and the notice says so. Branch logic itself is covered by
    // unit tests in `src/model/slice.rs`.
    cmd.current_dir(dir.path())
        .arg("slice")
        .arg(&target)
        .arg("--budget")
        .arg("0");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("exceeds --budget 0"))
        .stdout(predicate::str::contains("pub fn handle()"));
}

#[test]
fn test_cli_slice_ambiguous_error_suggests_retry_commands() {
    let dir = tempdir().unwrap();
    for sub in ["alpha", "beta"] {
        let d = dir.path().join(sub);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("mod.rs"), "pub fn handler() {}\n").unwrap();
    }

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path())
        .arg("slice")
        .arg("mod.rs:handler");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Ambiguous"))
        .stderr(predicate::str::contains("mimori slice '"));
}

#[test]
fn test_cli_slice_caller_ceiling() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("hub.rs");
    let code = r#"
pub fn target_fn() {}
pub fn caller_1() { target_fn(); }
pub fn caller_2() { target_fn(); }
pub fn caller_3() { target_fn(); }
pub fn caller_4() { target_fn(); }
pub fn caller_5() { target_fn(); }
pub fn caller_6() { target_fn(); }
pub fn caller_7() { target_fn(); }
"#;
    fs::write(&file_path, code).unwrap();

    let target = format!("{}:target_fn", file_path.to_str().unwrap());

    let mut cmd = Command::cargo_bin("mimori").unwrap();
    cmd.current_dir(dir.path()).arg("slice").arg(&target);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("- **1-Hop Callers**:"))
        .stdout(predicate::str::contains(
            "_5 of 7 callers shown (use mimori up for full list)_",
        ));
}

#[test]
fn test_slice_result_numbered_code_slices() {
    use mimori::model::SliceResult;

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("logic.rs");
    let code = "pub fn calculate() {\n    let a = 10;\n    let b = 20;\n    let _ = a + b;\n}\n";
    fs::write(&file_path, code).unwrap();

    let graph = mimori::storage::get_or_sync_graph(dir.path()).unwrap();
    let coord = mimori::model::Coordinate::parse(&format!("{}:calculate", file_path.display()))
        .unwrap()
        .normalize_against(dir.path());

    let slice: SliceResult = graph.build_slice(&coord, false, false).unwrap();
    let numbered_md = slice.to_markdown_numbered();

    assert!(numbered_md.contains("L1: pub fn calculate() {"));
    assert!(numbered_md.contains("L2:     let a = 10;"));
    assert!(numbered_md.contains("L3:     let b = 20;"));
    assert!(numbered_md.contains("L4:     let _ = a + b;"));
    assert!(numbered_md.contains("L5: }"));
}

#[test]
fn test_slice_result_numbered_line_range() {
    use mimori::model::SliceResult;

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("lines.rs");
    let code = "first\nsecond\nthird\nfourth\nfifth\n";
    fs::write(&file_path, code).unwrap();

    let graph = mimori::storage::get_or_sync_graph(dir.path()).unwrap();
    let coord = mimori::model::Coordinate::parse(&format!("{}:#L2-4", file_path.display()))
        .unwrap()
        .normalize_against(dir.path());

    let slice: SliceResult = graph.build_slice(&coord, false, false).unwrap();
    let numbered_md = slice.to_markdown_numbered();

    assert!(numbered_md.contains("L2: second"));
    assert!(numbered_md.contains("L3: third"));
    assert!(numbered_md.contains("L4: fourth"));
}

#[test]
fn test_cli_slice_rejects_outside_workspace_paths() {
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(ws.path().join("local.rs"), "pub fn inside() {}\n").unwrap();
    let secret = outside.path().join("secret.rs");
    fs::write(&secret, "pub fn leak_me() { let _ = \"TOP_SECRET\"; }\n").unwrap();

    // Line coordinate outside workspace must fail closed and never leak contents.
    let mut cmd_line = Command::cargo_bin("mimori").unwrap();
    cmd_line
        .current_dir(ws.path())
        .arg("slice")
        .arg(format!("{}:#L1-1", secret.display()));
    cmd_line
        .assert()
        .failure()
        .stdout(predicate::str::contains("TOP_SECRET").not());

    // Symbol coordinate outside workspace must also fail closed and never write external .mimori.
    let mut cmd_sym = Command::cargo_bin("mimori").unwrap();
    cmd_sym
        .current_dir(ws.path())
        .arg("slice")
        .arg(format!("{}:leak_me", secret.display()));
    cmd_sym
        .assert()
        .failure()
        .stdout(predicate::str::contains("TOP_SECRET").not());
    assert!(
        !outside.path().join(".mimori").exists(),
        "must never create .mimori outside workspace"
    );
}

#[test]
fn test_cli_slice_json_is_byte_deterministic() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("det.rs"),
        r#"
pub fn a() {}
pub fn b() {}
pub fn c() {}
pub fn d() {}
pub fn hub() {
    a(); b(); c(); d(); a(); c();
}
"#,
    )
    .unwrap();

    let mut outputs = Vec::new();
    for _ in 0..5 {
        let out = Command::cargo_bin("mimori")
            .unwrap()
            .current_dir(dir.path())
            .args(["--json", "slice", "det.rs:hub"])
            .output()
            .unwrap();
        assert!(out.status.success());
        outputs.push(out.stdout);
    }
    for out in &outputs[1..] {
        assert_eq!(
            out, &outputs[0],
            "slice --json output must be byte-deterministic"
        );
    }
}

#[test]
fn test_cli_slice_hash_symbol_and_line_zero_rejected() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn phase_one() {}\n").unwrap();

    // Documented `file#symbol` form must resolve cleanly.
    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["slice", "lib.rs#phase_one"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pub fn phase_one()"));

    // 1-indexed line coordinates must reject line 0.
    for bad in ["lib.rs:#L0", "lib.rs:#L0-0", "lib.rs:#L0-1"] {
        Command::cargo_bin("mimori")
            .unwrap()
            .current_dir(dir.path())
            .args(["slice", bad])
            .assert()
            .failure();
    }
}

#[test]
fn test_cli_slice_injects_jit_scoped_memory_notes() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src/db")).unwrap();
    fs::create_dir_all(dir.path().join(".agents")).unwrap();
    fs::write(
        dir.path().join("src/db/pool.rs"),
        "pub fn connect() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join(".agents/memory.md"),
        "\
# Project Memory

## Domain Vocabulary & Gotchas
- @scope:src/db/ Never open SQLite without busy_timeout.
- @sym:connect Always verify WAL checkpoint after writes.
- @scope:src/ui/ Unrelated frontend note must not leak into db slices.
",
    )
    .unwrap();

    Command::cargo_bin("mimori")
        .unwrap()
        .current_dir(dir.path())
        .args(["slice", "src/db/pool.rs:connect"])
        .assert()
        .success()
        .stdout(predicate::str::contains("busy_timeout"))
        .stdout(predicate::str::contains("WAL checkpoint"))
        .stdout(predicate::str::contains("Unrelated frontend note").not());
}
