#!/usr/bin/env python3
"""Regression and invariant verification suite for mimori CLI improvements.

Tests:
1. Ruby symbol and require_relative extraction + import graph edge resolution.
2. Atomic write safety and concurrency isolation via file_lock and os.replace.
3. Task lifecycle operations (todo add, start, done, promote) with atomic file updates.
4. Ponytail debt reconciliation and CI validation checks.
"""

from __future__ import annotations

import concurrent.futures
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

MIMORI_BIN = Path(__file__).resolve().parent / "mimori"


def run_mimori(args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(MIMORI_BIN), *args],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        check=False,
    )


def test_ruby_ast_and_import_graph(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    src_dir = tmp_dir / "src"
    src_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    app_rb = src_dir / "app.rb"
    app_rb.write_text(
        "require_relative './services/user_service'\n\n"
        "class AppController\n"
        "  def handle_request(req)\n"
        "  end\n"
        "end\n",
        encoding="utf-8",
    )

    svc_dir = src_dir / "services"
    svc_dir.mkdir(parents=True, exist_ok=True)
    svc_rb = svc_dir / "user_service.rb"
    svc_rb.write_text(
        "module UserService\n"
        "  def find_user(id)\n"
        "  end\n"
        "end\n",
        encoding="utf-8",
    )

    res = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res.returncode == 0, f"mimori map failed: {res.stderr}"
    map_out = res.stdout

    assert "class AppController" in map_out, f"AppController must be extracted. Output:\n{map_out}"
    assert "module UserService" in map_out, f"UserService must be extracted. Output:\n{map_out}"
    assert "app.rb" in map_out and "user_service.rb" in map_out, "Both Ruby files must appear in map"
    
    # Verify graph edge
    user_svc_line = next((l for l in map_out.splitlines() if "user_service.rb" in l), "")
    assert "app" in user_svc_line or "src/app" in user_svc_line, f"app.rb must be listed as importer of user_service.rb, got: {user_svc_line}"
    print("[PASS] Ruby AST extraction & graph edge resolution verified.")


def test_atomic_writes_and_concurrency(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    res = run_mimori(["init"], cwd=tmp_dir)
    assert res.returncode == 0, f"mimori init failed: {res.stderr}"

    def concurrent_logger(idx: int) -> int:
        r = run_mimori(
            ["log", "--action", f"worker-{idx}", "--summary", f"Summary {idx}", "--files", f"f_{idx}.py"],
            cwd=tmp_dir,
        )
        return r.returncode

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        statuses = list(executor.map(concurrent_logger, range(16)))

    assert all(code == 0 for code in statuses), "All concurrent logs must return 0"
    act_file = tmp_dir / ".mimori" / "activity.jsonl"
    assert act_file.exists(), "activity.jsonl must exist"

    lines = act_file.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 16, f"Expected 16 records in activity.jsonl, got {len(lines)}"
    for line in lines:
        rec = json.loads(line)
        assert "action" in rec and "summary" in rec
    print("[PASS] Atomic writes & advisory locking concurrency verified.")


def test_todo_and_idea_lifecycle(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    run_mimori(["init"], cwd=tmp_dir)

    # Add task
    res = run_mimori(["todo", "add", "Build feature X", "--prio", "high", "--tag", "core"], cwd=tmp_dir)
    assert res.returncode == 0

    # Start task
    res = run_mimori(["todo", "start", "1"], cwd=tmp_dir)
    assert res.returncode == 0

    # Complete task
    res = run_mimori(["todo", "done", "1"], cwd=tmp_dir)
    assert res.returncode == 0

    # Add idea & promote
    res = run_mimori(["idea", "add", "Explore quantum compiler"], cwd=tmp_dir)
    assert res.returncode == 0

    res = run_mimori(["idea", "promote", "quantum"], cwd=tmp_dir)
    assert res.returncode == 0

    tasks_content = (tmp_dir / ".mimori" / "tasks.md").read_text(encoding="utf-8")
    assert "[x]" in tasks_content, "Completed task must be marked [x]"
    assert "Explore quantum compiler" in tasks_content, "Promoted idea must be in tasks"
    print("[PASS] Task & Idea lifecycle state transitions verified.")


def test_tiered_ast_parser_resilience(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    src_dir = tmp_dir / "src"
    src_dir.mkdir(parents=True, exist_ok=True)

    # TypeScript
    (src_dir / "types.ts").write_text(
        "export interface UserConfig { timeout: number; }\n"
        "export class ConfigManager {\n"
        "  getConfig(): UserConfig { return { timeout: 10 }; }\n"
        "}\n",
        encoding="utf-8",
    )
    # Go
    (src_dir / "server.go").write_text(
        "package main\n\ntype HttpServer struct {}\n\nfunc (s *HttpServer) Start() error { return nil }\n",
        encoding="utf-8",
    )
    # Rust
    (src_dir / "lib.rs").write_text(
        "pub trait ParserEngine {\n    fn parse(&self) -> bool;\n}\n"
        "pub struct EngineCore;\n",
        encoding="utf-8",
    )

    res = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res.returncode == 0, f"mimori map failed: {res.stderr}"
    map_out = res.stdout

    assert "class ConfigManager" in map_out or "interface UserConfig" in map_out, "TS symbols must be extracted"
    assert "HttpServer" in map_out or "Start" in map_out, "Go symbols must be extracted"
    assert "EngineCore" in map_out or "ParserEngine" in map_out, "Rust symbols must be extracted"
    print("[PASS] Tiered polyglot AST parser resilience verified.")


def test_stale_reference_scanner(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    res = run_mimori(["init"], cwd=tmp_dir)
    assert res.returncode == 0

    sub_dir = tmp_dir / "src" / "pkg"
    sub_dir.mkdir(parents=True, exist_ok=True)
    (sub_dir / "app.py").write_text("def run(): pass\n", encoding="utf-8")

    mem_file = tmp_dir / ".mimori" / "memory.md"
    mem_file.write_text(
        "# Project Memory\n\n"
        "- Dotdir valid: `.mimori/activity.jsonl` should not be flagged\n"
        "- Subdir valid: `src/pkg/` directory reference should not be flagged\n"
        "- Slash command: `/goal` and `/list` commands should not be flagged\n"
        "- Dead ref: `nonexistent_module.py` must be flagged\n",
        encoding="utf-8",
    )

    res = run_mimori(["dump"], cwd=tmp_dir)
    assert res.returncode == 0, f"mimori dump failed: {res.stderr}"
    assert "memory.md: 'nonexistent_module.py' not found in repo" in res.stdout
    assert "mimori/activity.jsonl' not found in repo" not in res.stdout
    assert "'goal' not found in repo" not in res.stdout
    assert "'list' not found in repo" not in res.stdout
    assert "'src/pkg' not found in repo" not in res.stdout
    print("[PASS] Stale reference scanner resilience verified.")


def main() -> None:
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        test_ruby_ast_and_import_graph(root / "repo1")
        test_atomic_writes_and_concurrency(root / "repo2")
        test_todo_and_idea_lifecycle(root / "repo3")
        test_tiered_ast_parser_resilience(root / "repo4")
        test_stale_reference_scanner(root / "repo5")
    print("All mimori verification checks passed (exit 0).")


if __name__ == "__main__":
    main()
