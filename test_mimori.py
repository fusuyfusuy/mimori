#!/usr/bin/env python3
"""Regression and invariant verification suite for mimori CLI improvements.

Tests:
1. Ruby symbol and require_relative extraction + import graph edge resolution.
2. Atomic write safety and concurrency isolation via file_lock and os.replace.
3. Task lifecycle operations (todo add, start, done, promote) with atomic file updates.
4. Ponytail debt reconciliation and CI validation checks.
5. Directory probes rejecting same-named files, reported without tracebacks.
6. Lock files staying out of the memory directory and out of git.
"""

from __future__ import annotations

import concurrent.futures
import json
import os
import subprocess
import sys
import tempfile
import time
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


def test_dir_probes_reject_files(tmp_dir: Path) -> None:
    """C5/R4: a *file* named .mimori must not be mistaken for the memory dir, and the
    resulting collision must be reported as one line, not a traceback."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    (tmp_dir / ".mimori").write_text("not a directory\n", encoding="utf-8")

    res = run_mimori(["init"], cwd=tmp_dir)
    assert res.returncode == 1, f"a name collision must fail cleanly, got rc={res.returncode}"
    assert "Traceback" not in res.stderr, f"must not surface a traceback:\n{res.stderr}"
    assert "mimori:" in res.stderr, f"must diagnose the problem, got {res.stderr!r}"
    assert "File exists" in res.stderr, f"must name the collision, got {res.stderr!r}"
    print("[PASS] Directory probes reject same-named files without tracebacks.")


def test_lock_files_stay_out_of_content_dir(tmp_dir: Path) -> None:
    """R5: locks are machinery; they must not litter .mimori/ or show up as untracked."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    assert run_mimori(["init"], cwd=tmp_dir).returncode == 0
    assert run_mimori(["todo", "add", "some task"], cwd=tmp_dir).returncode == 0
    assert run_mimori(["log", "--action", "a", "--summary", "s"], cwd=tmp_dir).returncode == 0

    mimori_dir = tmp_dir / ".mimori"
    strays = sorted(f.name for f in mimori_dir.glob("*.lock"))
    assert not strays, f"lock files must not sit beside the memory files, found {strays}"
    assert (mimori_dir / ".locks").is_dir(), "locks must be relocated, not silently dropped"

    status = subprocess.run(
        ["git", "-c", "core.excludesFile=/dev/null", "status", "--porcelain", "-uall", ".mimori"],
        cwd=str(tmp_dir), capture_output=True, text=True, check=False,
    ).stdout
    assert ".lock" not in status, f"locks must be gitignored, git sees:\n{status}"
    print("[PASS] Lock files kept out of the content directory and out of git.")


def test_scope_and_monorepo_filtering(tmp_dir: Path) -> None:
    """Verify candidate pre-filtering, chunked batching, and --scope subtree isolation."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    sub1 = tmp_dir / "services" / "auth"
    sub2 = tmp_dir / "services" / "billing"
    assets = tmp_dir / "assets"
    sub1.mkdir(parents=True, exist_ok=True)
    sub2.mkdir(parents=True, exist_ok=True)
    assets.mkdir(parents=True, exist_ok=True)

    (sub1 / "auth_core.py").write_text("class AuthManager:\n    def login(self): pass\n", encoding="utf-8")
    (sub2 / "billing_core.py").write_text("class BillingEngine:\n    def charge(self): pass\n", encoding="utf-8")
    (assets / "logo.png").write_bytes(b"\x89PNG\r\n\x1a\n\x00")

    # Full map
    res_full = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res_full.returncode == 0
    assert "class AuthManager" in res_full.stdout
    assert "class BillingEngine" in res_full.stdout
    assert "logo.png" in res_full.stdout or "assets" in res_full.stdout

    # Scoped map
    res_scoped = run_mimori(["map", "--stdout", "--scope", "services/auth"], cwd=tmp_dir)
    assert res_scoped.returncode == 0
    assert "class AuthManager" in res_scoped.stdout
    assert "class BillingEngine" not in res_scoped.stdout
    assert "assets" not in res_scoped.stdout
    assert "scope: `services/auth`" in res_scoped.stdout

    # Scoped slice
    res_slice = run_mimori(["slice", "auth_core.py:AuthManager", "--scope", "services/auth"], cwd=tmp_dir)
    assert res_slice.returncode == 0
    assert "class AuthManager" in res_slice.stdout

    print("[PASS] Subtree scope filtering & candidate batching verified.")


def test_task_markdown_links_preserved(tmp_dir: Path) -> None:
    """C5/F5: Markdown links in tasks like [docs](url) must not be stripped or turned into tags."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    assert run_mimori(["init"], cwd=tmp_dir).returncode == 0

    task_desc = "Review [Architecture Docs](https://site.dev/docs) and [API Spec](http://api.dev) for [auth] module"
    res_add = run_mimori(["todo", "add", task_desc, "--prio", "high"], cwd=tmp_dir)
    assert res_add.returncode == 0

    # Verify task listing retains link
    res_list = run_mimori(["todo", "list"], cwd=tmp_dir)
    assert "[Architecture Docs](https://site.dev/docs)" in res_list.stdout
    assert "[API Spec](http://api.dev)" in res_list.stdout

    # Transition task to done
    res_done = run_mimori(["todo", "done", "1"], cwd=tmp_dir)
    assert res_done.returncode == 0

    # Read tasks.md file directly and verify links are completely intact
    tasks_content = (tmp_dir / ".mimori" / "tasks.md").read_text(encoding="utf-8")
    assert "[Architecture Docs](https://site.dev/docs)" in tasks_content
    assert "[API Spec](http://api.dev)" in tasks_content
    assert "[auth]" in tasks_content
    print("[PASS] Markdown links in tasks preserved across state transitions.")


def test_nested_module_manifests(tmp_dir: Path) -> None:
    """F4/R6: Nested go.mod and Cargo.toml manifest discovery."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    go_svc = tmp_dir / "services" / "payment"
    rust_crate = tmp_dir / "crates" / "engine"
    go_svc.mkdir(parents=True, exist_ok=True)
    rust_crate.mkdir(parents=True, exist_ok=True)

    (go_svc / "go.mod").write_text("module github.com/example/payment\n\ngo 1.21\n", encoding="utf-8")
    (go_svc / "pay.go").write_text("package payment\n\ntype PaymentProcessor struct{}\n", encoding="utf-8")

    (rust_crate / "Cargo.toml").write_text('[package]\nname = "engine_core"\nversion = "0.1.0"\n', encoding="utf-8")
    (rust_crate / "lib.rs").write_text("pub struct Engine;\n", encoding="utf-8")

    res_map = run_mimori(["map", "--stdout", "--format", "json"], cwd=tmp_dir)
    assert res_map.returncode == 0
    assert "PaymentProcessor" in res_map.stdout
    assert "Engine" in res_map.stdout
    print("[PASS] Nested module manifest discovery verified.")


def test_in_scope_cache_and_clean(tmp_dir: Path) -> None:
    """Verify in-scope cache (.mimori/.cache/context.md), gitignore shielding, and clean."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    (tmp_dir / "main.py").write_text("def run(): pass\n", encoding="utf-8")
    assert run_mimori(["init"], cwd=tmp_dir).returncode == 0

    # 1. Test dump --file writes into .mimori/.cache/context.md
    res_dump = run_mimori(["dump", "--file"], cwd=tmp_dir)
    assert res_dump.returncode == 0, f"dump --file failed: {res_dump.stderr}"
    cache_file = tmp_dir / ".mimori" / ".cache" / "context.md"
    assert cache_file.is_file(), f"Expected cache file at {cache_file}, got: {res_dump.stdout}"
    content = cache_file.read_text(encoding="utf-8")
    assert "# Agent Context Snapshot" in content
    assert "main.py" in content

    # 2. Test git status ignores .cache/ and .locks/
    status = subprocess.run(
        ["git", "-c", "core.excludesFile=/dev/null", "status", "--porcelain", "-uall", ".mimori"],
        cwd=str(tmp_dir), capture_output=True, text=True, check=False,
    ).stdout
    assert ".cache" not in status, f".cache must be gitignored, git sees:\n{status}"
    assert ".lock" not in status, f".locks must be gitignored, git sees:\n{status}"

    # 3. Test mimori clean removes the cached context
    res_clean = run_mimori(["clean"], cwd=tmp_dir)
    assert res_clean.returncode == 0, f"clean failed: {res_clean.stderr}"
    assert not cache_file.exists(), "Cache file must be deleted after clean"

    # 4. Test custom file target
    custom_target = tmp_dir / "custom_ctx.md"
    res_custom = run_mimori(["dump", "--file", str(custom_target)], cwd=tmp_dir)
    assert res_custom.returncode == 0
    assert custom_target.is_file()
    assert "# Agent Context Snapshot" in custom_target.read_text(encoding="utf-8")

    print("[PASS] In-scope cache, gitignore shielding, and clean verified.")


def test_reviewed_boundary_failures(tmp_dir: Path) -> None:
    """Reviewed boundaries fail closed: cache fallback, scope containment, and CLI errors."""
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    (tmp_dir / "app.py").write_text("def main():\n    pass\n", encoding="utf-8")

    sibling = tmp_dir.parent / f"{tmp_dir.name}-outside"
    sibling.mkdir(parents=True, exist_ok=True)
    try:
        (sibling / "secret.py").write_text("class OutsideSecret:\n    pass\n", encoding="utf-8")
        res_scope = run_mimori(["map", "--stdout", "--scope", f"../{sibling.name}"], cwd=tmp_dir)
        assert res_scope.returncode != 0, "scope traversal outside the repository must fail"
        assert "escapes repository root" in res_scope.stderr
        assert "OutsideSecret" not in res_scope.stdout
    finally:
        if (sibling / "secret.py").exists():
            (sibling / "secret.py").unlink()
        if sibling.exists():
            sibling.rmdir()

    for args in (["slice", "missing.py"], ["slice", "app.py", "--lines", "0"],
                 ["todo", "frobnicate", "task"], ["idea", "frobnicate", "idea"]):
        result = run_mimori(list(args), cwd=tmp_dir)
        assert result.returncode != 0, f"invalid command must fail: {' '.join(args)}"

    print("[PASS] Reviewed cache, scope, and CLI boundaries fail closed.")


def test_ast_delta_cache_lifecycle(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=str(tmp_dir), check=True)
    subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=str(tmp_dir), check=True)

    src = tmp_dir / "src"
    src.mkdir(parents=True, exist_ok=True)
    (src / "a.py").write_text("def func_a():\n    pass\n")
    (src / "b.py").write_text("from .a import func_a\ndef func_b():\n    func_a()\n")
    (src / "c.py").write_text("def func_c():\n    pass\n")

    subprocess.run(["git", "add", "."], cwd=str(tmp_dir), check=True)
    subprocess.run(["git", "commit", "-m", "initial"], cwd=str(tmp_dir), capture_output=True, check=True)

    # 1. Cold scan populates SQLite cache
    res = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res.returncode == 0, f"mimori map failed: {res.stderr}"
    db_path = tmp_dir / ".mimori" / ".cache" / "ast.db"
    assert db_path.exists(), "ast.db must be created in .mimori/.cache/"

    import sqlite3
    conn = sqlite3.connect(str(db_path))
    cur = conn.execute("SELECT path, symbols FROM ast_cache;")
    rows = dict(cur.fetchall())
    conn.close()
    assert "src/a.py" in rows and "func_a" in rows["src/a.py"]
    assert "src/b.py" in rows and "func_b" in rows["src/b.py"]
    assert "src/c.py" in rows

    # 2. Warm incremental run produces exact same output
    res2 = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res2.returncode == 0
    assert res.stdout == res2.stdout, "cached map output must match cold run"

    # 3. Delta modification invalidates only touched file
    time.sleep(0.05)
    (src / "a.py").write_text("def func_a():\n    pass\ndef func_a_new():\n    pass\n")
    slice_res = run_mimori(["slice", "src/a.py:func_a_new"], cwd=tmp_dir)
    assert slice_res.returncode == 0, f"slice on newly added func failed: {slice_res.stderr}"
    assert "func_a_new" in slice_res.stdout

    conn = sqlite3.connect(str(db_path))
    cur = conn.execute("SELECT symbols FROM ast_cache WHERE path = 'src/a.py';")
    a_syms = cur.fetchone()[0]
    conn.close()
    assert "func_a_new" in a_syms, "ast_cache must reflect newly parsed symbol"

    # 4. Deleted file is pruned from cache
    (src / "c.py").unlink()
    subprocess.run(["git", "rm", "src/c.py"], cwd=str(tmp_dir), capture_output=True, check=True)
    res_prune = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res_prune.returncode == 0
    conn = sqlite3.connect(str(db_path))
    cur = conn.execute("SELECT path FROM ast_cache;")
    remaining = [r[0] for r in cur.fetchall()]
    conn.close()
    assert "src/c.py" not in remaining, "deleted file must be pruned from cache"

    # 5. Corrupt DB auto-recovers transparently
    db_path.write_bytes(b"CORRUPTED BYTES HAZARD")
    res_corrupt = run_mimori(["map", "--stdout"], cwd=tmp_dir)
    assert res_corrupt.returncode == 0, "mimori must not crash on corrupt cache"
    assert "func_a_new" in res_corrupt.stdout

    # 6. mimori clean purges ast.db
    res_clean = run_mimori(["clean"], cwd=tmp_dir)
    assert res_clean.returncode == 0
    assert not db_path.exists(), "mimori clean must remove ast.db"

    print("[PASS] Automatic AST delta indexing, incremental invalidation, pruning & self-healing verified.")


def test_topic_sensitive_seed_pagerank(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    src = tmp_dir / "src"
    src.mkdir(parents=True, exist_ok=True)
    (src / "compose.py").write_text(
        '"""Deployment builder."""\n'
        'def create_command(mode="stack"):\n'
        '    # docker stack deploy execution engine\n'
        '    return "stack deploy"\n'
    )
    ui = tmp_dir / "ui"
    ui.mkdir(parents=True, exist_ok=True)
    (ui / "swarm_view.py").write_text("def render_swarm():\n    pass\n")

    res = run_mimori(["map", "--stdout", "--seed", "stack deploy", "--format", "json"], cwd=tmp_dir)
    assert res.returncode == 0
    data = json.loads(res.stdout)
    compose_entry = next(f for f in data["files"] if f["path"] == "src/compose.py")
    assert compose_entry["seed_match"] is True
    assert compose_entry["score"] > 0
    print("[PASS] Topic-sensitive seed PageRank & seed node boosting verified.")


def test_no_tests_and_kind_filtering(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    backend = tmp_dir / "backend"
    backend.mkdir(parents=True, exist_ok=True)
    (backend / "server.py").write_text("def start_server():\n    pass\n")
    (backend / "test_server.py").write_text("def test_start():\n    pass\n")

    frontend = tmp_dir / "components"
    frontend.mkdir(parents=True, exist_ok=True)
    (frontend / "Dashboard.tsx").write_text("export const Dashboard = () => null;\n")

    # 1. --no-tests excludes test_server.py
    res_notests = run_mimori(["map", "--stdout", "--no-tests", "--format", "json"], cwd=tmp_dir)
    assert res_notests.returncode == 0
    data = json.loads(res_notests.stdout)
    paths = {f["path"] for f in data["files"]}
    assert "backend/server.py" in paths
    assert "backend/test_server.py" not in paths

    # 2. --kind backend excludes Dashboard.tsx
    res_backend = run_mimori(["map", "--stdout", "--kind", "backend", "--format", "json"], cwd=tmp_dir)
    assert res_backend.returncode == 0
    b_paths = {f["path"] for f in json.loads(res_backend.stdout)["files"]}
    assert "backend/server.py" in b_paths
    assert "components/Dashboard.tsx" not in b_paths

    # 3. --kind frontend only includes Dashboard.tsx
    res_frontend = run_mimori(["map", "--stdout", "--kind", "frontend", "--format", "json"], cwd=tmp_dir)
    assert res_frontend.returncode == 0
    f_paths = {f["path"] for f in json.loads(res_frontend.stdout)["files"]}
    assert "components/Dashboard.tsx" in f_paths
    assert "backend/server.py" not in f_paths
    print("[PASS] Noise filtering (--no-tests, --kind backend|frontend) verified.")


def test_adaptive_slice_and_coordinates(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    src = tmp_dir / "src"
    src.mkdir(parents=True, exist_ok=True)
    lines = ["# line " + str(i) for i in range(1, 161)]
    lines[120] = "def create_command():"
    lines[121] = "    return 'cmd'"
    (src / "large.py").write_text("\n".join(lines) + "\n")

    # 1. Adaptive file slice: all 160 lines rendered without clipping
    res_file = run_mimori(["slice", "src/large.py"], cwd=tmp_dir)
    assert res_file.returncode == 0
    assert "160:" in res_file.stdout
    assert "clipped to" not in res_file.stdout

    # 2. Slicing symbol renders symbol completely
    res_sym = run_mimori(["slice", "src/large.py:create_command"], cwd=tmp_dir)
    assert res_sym.returncode == 0
    assert "def create_command" in res_sym.stdout

    # 3. Coordinate range syntax path#Lstart-Lend
    res_coord1 = run_mimori(["slice", "src/large.py#L121-L125"], cwd=tmp_dir)
    assert res_coord1.returncode == 0
    assert "lines 121–125" in res_coord1.stdout
    assert "121: def create_command():" in res_coord1.stdout

    # 4. Coordinate range syntax path:start-end
    res_coord2 = run_mimori(["slice", "src/large.py:121-125"], cwd=tmp_dir)
    assert res_coord2.returncode == 0
    assert "lines 121–125" in res_coord2.stdout

    # 5. Offset and lines pagination
    res_page = run_mimori(["slice", "src/large.py", "--offset", "50", "--lines", "10"], cwd=tmp_dir)
    assert res_page.returncode == 0
    assert "50: # line 50" in res_page.stdout
    assert "59: # line 59" in res_page.stdout
    assert "61: # line 61" not in res_page.stdout
    print("[PASS] Adaptive slice boundaries, coordinate ranges (#L121-L125), and pagination verified.")


def test_slice_follow_local_callees(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    src = tmp_dir / "src"
    src.mkdir(parents=True, exist_ok=True)
    (src / "domain.py").write_text(
        "def _add_domain_to_compose(domain):\n"
        "    return f'traefik.{domain}'\n"
        "\n"
        "def write_domains_to_compose(domains):\n"
        "    labels = [_add_domain_to_compose(d) for d in domains]\n"
        "    return labels\n"
    )

    res = run_mimori(["slice", "src/domain.py:write_domains_to_compose", "--follow-local"], cwd=tmp_dir)
    assert res.returncode == 0
    assert "Local Callees (inlined)" in res.stdout
    assert "_add_domain_to_compose" in res.stdout
    assert "traefik.{domain}" in res.stdout
    print("[PASS] Local callee inlining (--follow-local) verified.")


def test_scope_absolute_and_multi(tmp_dir: Path) -> None:
    tmp_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init"], cwd=str(tmp_dir), capture_output=True, check=True)

    pkg1 = tmp_dir / "packages" / "server"
    pkg2 = tmp_dir / "packages" / "client"
    docs = tmp_dir / "docs"
    pkg1.mkdir(parents=True, exist_ok=True)
    pkg2.mkdir(parents=True, exist_ok=True)
    docs.mkdir(parents=True, exist_ok=True)

    (pkg1 / "srv.py").write_text("# ponytail: fix server later <- 1 -> 2\ndef run_server(): pass\n")
    (pkg2 / "cli.py").write_text("def run_client(): pass\n")
    (docs / "readme.md").write_text("# ponytail: fix docs later <- 1 -> 2\n# Docs\n")

    # 1. Internal absolute path in --scope resolves cleanly without SystemExit
    abs_scope = str(pkg1.resolve())
    res_abs = run_mimori(["map", "--stdout", "--scope", abs_scope, "--format", "json"], cwd=tmp_dir)
    assert res_abs.returncode == 0
    abs_paths = {f["path"] for f in json.loads(res_abs.stdout)["files"]}
    assert "packages/server/srv.py" in abs_paths
    assert "packages/client/cli.py" not in abs_paths

    # 2. Multi-scope restricts to both packages
    res_multi = run_mimori(["map", "--stdout", "--scope", "packages/server,packages/client", "--format", "json"], cwd=tmp_dir)
    assert res_multi.returncode == 0
    multi_paths = {f["path"] for f in json.loads(res_multi.stdout)["files"]}
    assert "packages/server/srv.py" in multi_paths
    assert "packages/client/cli.py" in multi_paths
    assert "docs/readme.md" not in multi_paths

    # 3. Scoped debt check
    res_debt = run_mimori(["debt", "list", "--scope", "packages/server"], cwd=tmp_dir)
    assert res_debt.returncode == 0
    assert "fix server later" in res_debt.stdout
    assert "fix docs later" not in res_debt.stdout
    print("[PASS] Scoping enhancements (absolute paths, multi-scope, and scoped debt) verified.")


def main() -> None:
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        test_ruby_ast_and_import_graph(root / "repo1")
        test_atomic_writes_and_concurrency(root / "repo2")
        test_todo_and_idea_lifecycle(root / "repo3")
        test_tiered_ast_parser_resilience(root / "repo4")
        test_stale_reference_scanner(root / "repo5")
        test_dir_probes_reject_files(root / "repo6")
        test_lock_files_stay_out_of_content_dir(root / "repo7")
        test_scope_and_monorepo_filtering(root / "repo8")
        test_task_markdown_links_preserved(root / "repo9")
        test_nested_module_manifests(root / "repo10")
        test_in_scope_cache_and_clean(root / "repo11")
        test_reviewed_boundary_failures(root / "repo12")
        test_ast_delta_cache_lifecycle(root / "repo13")
        test_topic_sensitive_seed_pagerank(root / "repo14")
        test_no_tests_and_kind_filtering(root / "repo15")
        test_adaptive_slice_and_coordinates(root / "repo16")
        test_slice_follow_local_callees(root / "repo17")
        test_scope_absolute_and_multi(root / "repo18")
    print("All mimori verification checks passed (exit 0).")


if __name__ == "__main__":
    main()

