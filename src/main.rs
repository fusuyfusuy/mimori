use anyhow::{bail, Result};
use clap::Parser;
use mimori::cli::{
    BlastArgs, CleanArgs, Cli, Commands, DebtArgs, DebtCommand, DiffArgs, DoctorArgs, DumpArgs,
    FindArgs, InitArgs, MapArgs, MemoryArgs, MemoryCommand, MissingArgs, SliceArgs, TargetArgs,
};
use mimori::graph::blast::{format_sink_hits, parse_sink_list, sweep_literal_sinks};
use mimori::graph::map::{generate_map, personalize_map, validate_map_scope};
use mimori::graph::{slice_line_coordinate, zero_caller_hint, SymbolGraph};
use mimori::memory::{
    check_debt, generate_dump, list_debt, sync_debt, MemoryLedger, MAX_DEBT_CEILING,
};
use mimori::model::Coordinate;
use mimori::storage::{clean_cache, get_or_sync_graph};
use mimori::workspace::walker::{
    confine_to_workspace, find_workspace_root, is_disallowed_workspace_root,
};
use serde::Serialize;
use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ws = cli.workspace.as_deref();

    match cli.command {
        Commands::Slice(args) => cmd_slice(args, ws, &current_dir, cli.json),
        Commands::Find(args) => cmd_find(args, ws, &current_dir, cli.json),
        Commands::Up(args) => cmd_up(args, ws, &current_dir, cli.json),
        Commands::Down(args) => cmd_down(args, ws, &current_dir, cli.json),
        Commands::Uses(args) => cmd_uses(args, ws, &current_dir, cli.json),
        Commands::Missing(args) => cmd_missing(args, ws, &current_dir, cli.json),
        Commands::Map(args) => cmd_map(args, ws, &current_dir, cli.json),
        Commands::Diff(args) => cmd_diff(args, ws, &current_dir, cli.json),
        Commands::Blast(args) => cmd_blast(args, ws, &current_dir, cli.json),
        Commands::Doctor(args) => cmd_doctor(args, ws, &current_dir, cli.json),
        Commands::Clean(args) => cmd_clean(args, ws, &current_dir, cli.json),
        Commands::Init(args) => cmd_init(args, ws, &current_dir, cli.json),
        Commands::Memory(args) => cmd_memory(args, ws, &current_dir, cli.json),
        Commands::Debt(args) => cmd_debt(args, ws, &current_dir, cli.json),
        Commands::Dump(args) => cmd_dump(args, ws, &current_dir, cli.json),
        Commands::Mcp(_) => match mimori::mcp::run_mcp_server(cli.workspace) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error running MCP server: {}", e);
                ExitCode::FAILURE
            }
        },
    }
}

fn print_json<T: Serialize>(value: &T) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{}", text);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error serializing JSON: {}", e);
            ExitCode::FAILURE
        }
    }
}

/// Resolve workspace root strictly from `--workspace` or `cwd` — never from
/// untrusted coordinate paths (prevents CLI workspace-confinement bypass).
fn resolve_cli_root(workspace: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let root = match workspace {
        Some(w) => {
            let candidate = if w.is_absolute() {
                w.to_path_buf()
            } else {
                cwd.join(w)
            };
            candidate
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Invalid --workspace '{}': {}", w.display(), e))?
        }
        None => find_workspace_root(None, cwd),
    };
    if is_disallowed_workspace_root(&root) {
        bail!(
            "Cannot access home or system directory '{}' as workspace",
            root.display()
        );
    }
    Ok(root)
}

/// Confine any file path inside `coord` against `root` (resolving relative
/// subdir paths like `../lib.rs` against `cwd` before confining to `root`).
fn confine_and_normalize_coord(coord: Coordinate, root: &Path, cwd: &Path) -> Result<Coordinate> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let Some(file) = coord.file() else {
        return Ok(coord);
    };
    let has_parent = file
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));

    let confined = if file.is_absolute() {
        Some(confine_to_workspace(&canon_root, file)?)
    } else if has_parent {
        Some(confine_to_workspace(&canon_root, &cwd.join(file))?)
    } else {
        let from_cwd = cwd.join(file);
        let from_root = canon_root.join(file);
        if fs::symlink_metadata(&from_cwd).is_ok() {
            Some(confine_to_workspace(&canon_root, &from_cwd)?)
        } else if fs::symlink_metadata(&from_root).is_ok() {
            Some(confine_to_workspace(&canon_root, &from_root)?)
        } else {
            None
        }
    };

    if let Some(abs_target) = confined {
        let rel = abs_target
            .strip_prefix(&canon_root)
            .unwrap_or(&abs_target)
            .to_path_buf();
        return Ok(match coord {
            Coordinate::Symbol { name, .. } => Coordinate::Symbol { file: rel, name },
            Coordinate::Lines { start, end, .. } => Coordinate::Lines {
                file: rel,
                start,
                end,
            },
            other => other,
        });
    }

    Ok(coord.normalize_against(&canon_root))
}

fn cmd_slice(args: SliceArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let mut cached_graph: Option<SymbolGraph> = None;
    let mut slices = Vec::with_capacity(args.targets.len());

    for raw_target in &args.targets {
        let res = (|| -> Result<mimori::model::SliceResult> {
            let parsed = Coordinate::parse(raw_target)?;
            let norm = confine_and_normalize_coord(parsed, &root, cwd)?;
            if let Coordinate::Lines { file, start, end } = &norm {
                let target_path = confine_to_workspace(&root, file)?;
                return slice_line_coordinate(
                    &target_path,
                    *start,
                    *end,
                    args.with_imports,
                    Some(&root),
                );
            }
            if cached_graph.is_none() {
                cached_graph = Some(get_or_sync_graph(&root)?);
            }
            let graph = cached_graph
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("workspace graph unavailable"))?;
            graph.build_slice(&norm, args.follow_local, args.with_imports)
        })();
        match res {
            Ok(s) => slices.push(s),
            Err(e) => {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    if as_json {
        if args.budget.is_some() {
            eprintln!("Warning: --budget only affects markdown output; ignoring for --json.");
        }
        return if slices.len() == 1 {
            print_json(&slices[0])
        } else {
            print_json(&slices)
        };
    }

    let per_budget = args.budget.map(|b| b / slices.len().max(1));
    let rendered: Vec<String> = slices
        .iter()
        .map(|s| match per_budget {
            Some(b) => s.render_markdown_budgeted(b, args.numbered),
            None => s.render_markdown(args.numbered),
        })
        .collect();
    print!("{}", rendered.join("\n---\n"));
    ExitCode::SUCCESS
}

fn cmd_find(args: FindArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let res = match mimori::workspace::execute_find(
        &root,
        &args.pattern,
        args.symbols_only,
        args.files_only,
        args.scope.as_deref(),
        args.limit,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if as_json {
        print_json(&res)
    } else {
        print!("{}", res.to_markdown());
        ExitCode::SUCCESS
    }
}

fn load_target_graph(
    raw: &str,
    ws: Option<&Path>,
    cwd: &Path,
) -> Result<(SymbolGraph, Coordinate)> {
    let root = resolve_cli_root(ws, cwd)?;
    let coord = confine_and_normalize_coord(Coordinate::parse(raw)?, &root, cwd)?;
    let graph = get_or_sync_graph(&root)?;
    Ok((graph, coord))
}

fn cmd_up(args: TargetArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let (graph, coord) = match load_target_graph(&args.target, ws, cwd) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let (callers, value_uses) = match graph.upstream_strict(&coord) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if as_json {
        let json_callers: Vec<_> = callers.iter().map(|s| s.coordinate()).collect();
        let json_uses: Vec<_> = value_uses.iter().map(|s| s.coordinate()).collect();
        print_json(&json!({
            "target": coord.to_string(),
            "callers": json_callers,
            "value_uses": json_uses
        }))
    } else {
        print!(
            "{}",
            mimori::graph::format_upstream(&graph, &coord, &callers, &value_uses)
        );
        ExitCode::SUCCESS
    }
}

fn cmd_down(args: TargetArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let (graph, coord) = match load_target_graph(&args.target, ws, cwd) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let callees = match graph.callees_strict(&coord) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if as_json {
        let json_callees: Vec<_> = callees.iter().map(|s| s.coordinate()).collect();
        print_json(&json!({ "target": coord.to_string(), "callees": json_callees }))
    } else {
        print!(
            "{}",
            mimori::graph::format_downstream(&graph, &coord, &callees)
        );
        ExitCode::SUCCESS
    }
}

fn cmd_uses(args: TargetArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let (graph, coord) = match load_target_graph(&args.target, ws, cwd) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let mentioners = match graph.mentioners_strict(&coord) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if as_json {
        let json_mentioners: Vec<_> = mentioners.iter().map(|s| s.coordinate()).collect();
        print_json(&json!({ "target": coord.to_string(), "mentioners": json_mentioners }))
    } else {
        print!(
            "{}",
            mimori::graph::format_uses(&graph, &coord, &mentioners)
        );
        ExitCode::SUCCESS
    }
}

fn cmd_missing(args: MissingArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match mimori::graph::missing::find_missing(
        &root,
        args.scope.as_deref(),
        &args.pattern,
        args.defines.as_deref(),
    ) {
        Ok(missing) => {
            if as_json {
                print_json(&json!({
                    "pattern": args.pattern,
                    "scope": args.scope,
                    "defines": args.defines,
                    "missing": missing,
                }))
            } else {
                print!(
                    "{}",
                    mimori::graph::missing::format_missing_markdown(
                        &args.pattern,
                        args.scope.as_deref(),
                        args.defines.as_deref(),
                        &missing,
                    )
                );
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn cmd_map(args: MapArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let validated_scope = match validate_map_scope(&root, args.scope.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let mut graph = match get_or_sync_graph(&root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error syncing workspace: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = personalize_map(
        &mut graph,
        args.focus.as_deref(),
        args.seed.as_deref(),
        &root,
    ) {
        eprintln!("Error: {}", e);
        return ExitCode::FAILURE;
    }

    let map_result = generate_map(
        &graph,
        validated_scope.as_deref(),
        args.focus.as_deref(),
        args.limit,
    );
    if as_json {
        print_json(&map_result)
    } else {
        print!("{}", map_result.to_markdown());
        ExitCode::SUCCESS
    }
}

fn cmd_diff(args: DiffArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    if let Err(e) = mimori::graph::diff::validate_diff_base(args.base.as_deref()) {
        eprintln!("Error: {}", e);
        return ExitCode::FAILURE;
    }
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let graph = match get_or_sync_graph(&root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error syncing workspace: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let report = match mimori::graph::diff::calculate_diff_blast(
        &root,
        &graph,
        args.staged,
        args.base.as_deref(),
        args.depth,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if as_json {
        print_json(&report)
    } else {
        print!("{}", report.to_markdown());
        ExitCode::SUCCESS
    }
}

fn cmd_blast(args: BlastArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let graph = match get_or_sync_graph(&root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error syncing workspace: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let sinks = parse_sink_list(args.with_sinks.as_deref());
    let mut json_items = Vec::with_capacity(args.targets.len());
    let mut md_blocks = Vec::with_capacity(args.targets.len());

    for raw_target in &args.targets {
        let coord = match Coordinate::parse(raw_target)
            .and_then(|c| confine_and_normalize_coord(c, &root, cwd))
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }
        };
        let blast_res = if args.down {
            mimori::graph::blast::calculate_downstream_blast(&graph, &coord, args.depth)
        } else {
            mimori::graph::blast::calculate_blast_radius(&graph, &coord, args.depth)
        };
        let blast_res = match blast_res {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }
        };

        let target_file = graph.resolve(&coord).into_first_file(&graph);
        let cone = blast_res.cone_files(target_file.as_deref());
        let (sink_hits, sink_truncated) = if sinks.is_empty() {
            (Vec::new(), false)
        } else {
            sweep_literal_sinks(&root, &graph, &sinks, Some(&cone))
        };

        if as_json {
            let mut v = match serde_json::to_value(&blast_res) {
                Ok(val) => val,
                Err(e) => {
                    eprintln!("Error serializing JSON: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            if !sinks.is_empty() {
                v["sinks"] = json!(sinks);
                v["sink_hits"] = json!(sink_hits);
                if sink_truncated {
                    v["sink_hits_truncated"] = json!(true);
                }
            }
            json_items.push(v);
        } else {
            md_blocks.push(render_cli_blast_markdown(
                &blast_res,
                &graph,
                &coord,
                args.down,
                &sinks,
                &sink_hits,
                sink_truncated,
            ));
        }
    }

    if as_json {
        if json_items.len() == 1 {
            print_json(&json_items[0])
        } else {
            print_json(&json_items)
        }
    } else {
        print!("{}", md_blocks.join("\n---\n"));
        ExitCode::SUCCESS
    }
}

trait ResolutionExt {
    fn into_first_file(self, graph: &SymbolGraph) -> Option<String>;
}

impl ResolutionExt for mimori::graph::Resolution {
    fn into_first_file(self, graph: &SymbolGraph) -> Option<String> {
        match self {
            mimori::graph::Resolution::Unique(i) => Some(graph.symbols[i].file.clone()),
            mimori::graph::Resolution::Ambiguous(v) => {
                v.first().map(|&i| graph.symbols[i].file.clone())
            }
            mimori::graph::Resolution::NotFound => None,
        }
    }
}

fn cmd_doctor(args: DoctorArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let graph = match get_or_sync_graph(&root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error syncing workspace: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let res = mimori::graph::doctor::run_doctor(&graph, args.limit);
    if as_json {
        print_json(&res)
    } else {
        print!("{}", res.to_markdown());
        ExitCode::SUCCESS
    }
}

fn cmd_clean(args: CleanArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match clean_cache(&root, args.all) {
        Ok(_) => {
            if as_json {
                print_json(&json!({ "cleaned": true, "all": args.all }))
            } else {
                println!("Cleaned .mimori cache.");
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("Error cleaning cache: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn cmd_init(args: InitArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match ws {
        Some(w) if w.is_absolute() => w.to_path_buf(),
        Some(w) => cwd.join(w),
        None => cwd.to_path_buf(),
    };
    if is_disallowed_workspace_root(&root) {
        eprintln!(
            "INIT_FAIL: Cannot initialize mimori in home or system directory '{}'; exit 1.",
            root.display()
        );
        return ExitCode::FAILURE;
    }

    // Validate all target paths BEFORE any filesystem mutation (atomic init).
    let mimori_dir = root.join(".mimori");
    if fs::symlink_metadata(&mimori_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        eprintln!(
            "INIT_FAIL: Refusing to follow symlink '{}'; exit 1.",
            mimori_dir.display()
        );
        return ExitCode::FAILURE;
    }
    let gitignore_path = root.join(".gitignore");
    if fs::symlink_metadata(&gitignore_path).is_ok_and(|m| m.file_type().is_symlink()) {
        eprintln!(
            "INIT_FAIL: Refusing to modify symlinked .gitignore '{}'; exit 1.",
            gitignore_path.display()
        );
        return ExitCode::FAILURE;
    }

    let mem_created = match MemoryLedger::scaffold(&root, args.force) {
        Ok(created) => created,
        Err(e) => {
            eprintln!("INIT_FAIL: scaffold .agents: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = fs::create_dir_all(&mimori_dir) {
        eprintln!(
            "INIT_FAIL: create dir {}: {}; exit 1.",
            mimori_dir.display(),
            e
        );
        return ExitCode::FAILURE;
    }

    let gitignore_status = if gitignore_path.exists() {
        let content = match fs::read_to_string(&gitignore_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("INIT_FAIL: read .gitignore: {}; exit 1.", e);
                return ExitCode::FAILURE;
            }
        };
        if content
            .lines()
            .any(|l| l.trim() == ".mimori" || l.trim() == ".mimori/")
        {
            "already configured"
        } else {
            let mut updated = content;
            if !updated.ends_with('\n') && !updated.is_empty() {
                updated.push('\n');
            }
            updated.push_str(".mimori/\n");
            if let Err(e) = fs::write(&gitignore_path, updated) {
                eprintln!("INIT_FAIL: write .gitignore: {}; exit 1.", e);
                return ExitCode::FAILURE;
            }
            "updated"
        }
    } else {
        if let Err(e) = fs::write(&gitignore_path, ".mimori/\n") {
            eprintln!("INIT_FAIL: create .gitignore: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
        "created"
    };

    let agents_status = if mem_created {
        "initialized"
    } else {
        "verified"
    };

    if as_json {
        print_json(&json!({
            "initialized": true,
            "mimori_cache": mimori_dir.display().to_string(),
            "gitignore": gitignore_status,
            "agents": agents_status,
        }))
    } else {
        println!(
            "INIT: .mimori cache ready; .gitignore {}; .agents/ {} (memory.md); exit 0.",
            gitignore_status, agents_status
        );
        ExitCode::SUCCESS
    }
}

fn cmd_memory(args: MemoryArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("MEM_ERR: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(MemoryCommand::Lint) | Some(MemoryCommand::Check) => {
            let ledger = match MemoryLedger::load(&root) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("MEM_LINT_FAIL: load .agents/memory.md: {}; exit 1.", e);
                    return ExitCode::FAILURE;
                }
            };
            let report = ledger.lint();
            if as_json {
                let code = print_json(&report);
                if code != ExitCode::SUCCESS {
                    return code;
                }
            } else {
                println!("{}", report.to_m2m_output());
            }
            if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some(MemoryCommand::Add(add_args)) => {
            let mem_path = MemoryLedger::memory_path(&root);
            if !mem_path.exists() {
                if let Err(e) = MemoryLedger::scaffold(&root, false) {
                    eprintln!("MEM_ADD_FAIL: scaffold .agents/memory.md: {}; exit 1.", e);
                    return ExitCode::FAILURE;
                }
            }
            let mut ledger = match MemoryLedger::load(&root) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("MEM_ADD_FAIL: load .agents/memory.md: {}; exit 1.", e);
                    return ExitCode::FAILURE;
                }
            };
            match ledger.add_entry(
                &add_args.section,
                &add_args.text,
                add_args.scope.as_deref(),
                add_args.sym.as_deref(),
            ) {
                Ok(entry) => {
                    if as_json {
                        print_json(&json!({
                            "added": true,
                            "section": add_args.section,
                            "entry": entry,
                        }))
                    } else {
                        println!(
                            "MEM_ADD: appended to '{}': {}; exit 0.",
                            add_args.section, entry
                        );
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => {
                    eprintln!("MEM_ADD_FAIL: {}; exit 1.", e);
                    ExitCode::FAILURE
                }
            }
        }
        Some(MemoryCommand::Resolve(res_args)) => {
            let mut ledger = match MemoryLedger::load(&root) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("MEM_RESOLVE_FAIL: load .agents/memory.md: {}; exit 1.", e);
                    return ExitCode::FAILURE;
                }
            };
            match ledger.resolve(&res_args.pattern, res_args.all) {
                Ok(deleted_lines) => {
                    let deleted = deleted_lines.len();
                    if as_json {
                        print_json(&json!({
                            "deleted": deleted,
                            "deleted_lines": deleted_lines,
                            "pattern": res_args.pattern,
                            "remaining_debt": ledger.raw_debt_lines.len(),
                            "ceiling": MAX_DEBT_CEILING
                        }))
                    } else {
                        println!(
                            "MEM_RESOLVE: deleted {} lines matching '{}'; debt: {}/{} lines; exit 0.",
                            deleted,
                            res_args.pattern,
                            ledger.raw_debt_lines.len(),
                            MAX_DEBT_CEILING
                        );
                        for line in &deleted_lines {
                            println!("  removed: {}", line);
                        }
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => {
                    eprintln!("MEM_RESOLVE_FAIL: {}; exit 1.", e);
                    ExitCode::FAILURE
                }
            }
        }
        Some(MemoryCommand::Show(show_args)) => {
            let sec = show_args.section.as_deref().or(args.section.as_deref());
            let budget = show_args.budget.or(args.budget);
            show_memory(&root, sec, budget, as_json)
        }
        None => show_memory(&root, args.section.as_deref(), args.budget, as_json),
    }
}

fn show_memory(root: &Path, sec: Option<&str>, budget: Option<usize>, as_json: bool) -> ExitCode {
    let ledger = match MemoryLedger::load(root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("MEM_SHOW_FAIL: load .agents/memory.md: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
    };
    if as_json {
        let val = if let Some(s) = sec {
            let content = ledger.get_section(s);
            json!({
                "section": s,
                "found": content.is_some(),
                "content": content.unwrap_or_default(),
            })
        } else {
            json!({
                "epics": ledger.epics,
                "debt_count": ledger.raw_debt_lines.len(),
                "debt_items": ledger.debt_items,
                "vocab": ledger.vocab,
                "gotchas": ledger.gotchas,
                "vocab_gotchas": ledger.vocab_gotchas,
            })
        };
        return print_json(&val);
    }
    if let Some(s) = sec {
        match ledger.get_section(s) {
            Some(content) => print_budgeted(&content, budget),
            None => println!("MEM_EMPTY: section '{}' not found; exit 0.", s),
        }
    } else {
        print_budgeted(&ledger.raw_content, budget);
    }
    ExitCode::SUCCESS
}

fn cmd_debt(args: DebtArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("DEBT_ERR: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(DebtCommand::Check) => {
            let (passed, output) = check_debt(&root);
            if as_json {
                let code = print_json(&json!({ "passed": passed, "output": output }));
                if code != ExitCode::SUCCESS {
                    return code;
                }
            } else {
                println!("{}", output);
            }
            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some(DebtCommand::Sync) => match sync_debt(&root) {
            Ok((in_code, manual, synced, output)) => {
                if as_json {
                    print_json(&json!({
                        "in_code": in_code,
                        "manual": manual,
                        "synced": synced,
                        "ceiling": MAX_DEBT_CEILING
                    }))
                } else {
                    println!("{}", output);
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("DEBT_SYNC_FAIL: {}; exit 1.", e);
                ExitCode::FAILURE
            }
        },
        Some(DebtCommand::List(list_args)) => {
            let scope = list_args.scope.as_deref().or(args.scope.as_deref());
            run_debt_list(&root, scope, as_json)
        }
        None => run_debt_list(&root, args.scope.as_deref(), as_json),
    }
}

fn run_debt_list(root: &Path, scope: Option<&str>, as_json: bool) -> ExitCode {
    match list_debt(root, scope) {
        Ok((markers, output)) => {
            if as_json {
                print_json(&markers)
            } else {
                println!("{}", output);
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("DEBT_ERR: {}; exit 1.", e);
            ExitCode::FAILURE
        }
    }
}

fn cmd_dump(args: DumpArgs, ws: Option<&Path>, cwd: &Path, as_json: bool) -> ExitCode {
    let root = match resolve_cli_root(ws, cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("DUMP_FAIL: {}; exit 1.", e);
            return ExitCode::FAILURE;
        }
    };
    match generate_dump(&root, args.budget, args.focus.as_deref()) {
        Ok(dump) => {
            if as_json {
                print_json(&dump)
            } else {
                print!("{}", dump.markdown);
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("DUMP_FAIL: {}; exit 1.", e);
            ExitCode::FAILURE
        }
    }
}

fn render_cli_blast_markdown(
    blast_res: &mimori::graph::blast::BlastResult,
    graph: &SymbolGraph,
    coord: &Coordinate,
    down: bool,
    sinks: &[String],
    sink_hits: &[String],
    sink_truncated: bool,
) -> String {
    let mut out = blast_res.to_markdown();
    if blast_res.affected.is_empty() && blast_res.value_uses.is_empty() && !down {
        if let Some(hint) = zero_caller_hint(graph, coord) {
            out.push_str(&hint);
            out.push('\n');
        }
    }
    if !sinks.is_empty() {
        out.push_str(&format_sink_hits(sinks, sink_hits, sink_truncated));
    } else {
        out.push_str("\nTip: blast follows call edges plus labeled value-use rows — raw literals (e.g. console.*, logPath) need a sweep: `mimori blast <target> --with-sinks console.,logPath` or `rg`.\n");
    }
    out
}

fn print_budgeted(text: &str, budget: Option<usize>) {
    if let Some(b) = budget {
        let max_chars = b.saturating_mul(4);
        if text.chars().count() > max_chars {
            let mut acc = 0usize;
            for line in text.lines() {
                let line_chars = line.chars().count() + 1;
                if acc.saturating_add(line_chars) > max_chars {
                    println!("... [truncated to fit token budget]");
                    break;
                }
                println!("{}", line);
                acc = acc.saturating_add(line_chars);
            }
            return;
        }
    }
    print!("{}", text);
    if !text.ends_with('\n') {
        println!();
    }
}
