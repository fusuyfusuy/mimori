use anyhow::{bail, Result};
use clap::Parser;
use mimori::cli::{Cli, Commands, DebtCommand, MemoryCommand};
use mimori::graph::blast::{format_sink_hits, parse_sink_list, sweep_literal_sinks};
use mimori::graph::map::{generate_map, personalize_map};
use mimori::graph::{slice_line_coordinate, zero_caller_hint, SymbolGraph};
use mimori::memory::{check_debt, generate_dump, list_debt, sync_debt, MemoryLedger};
use mimori::model::Coordinate;
use mimori::storage::{clean_cache, get_or_sync_graph};
use mimori::workspace::walker::find_workspace_root;
use serde_json::json;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let current_dir = env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match cli.command {
        Commands::Slice(args) => {
            let mut cached_graph: Option<(std::path::PathBuf, SymbolGraph)> = None;
            let mut slices = Vec::with_capacity(args.targets.len());

            for raw_target in &args.targets {
                match build_cli_slice(
                    raw_target,
                    &current_dir,
                    args.follow_local,
                    args.with_imports,
                    &mut cached_graph,
                ) {
                    Ok(s) => slices.push(s),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            }

            if cli.json {
                if args.budget.is_some() {
                    eprintln!(
                        "Warning: --budget only affects markdown output; ignoring for --json."
                    );
                }
                let out = if slices.len() == 1 {
                    serde_json::to_string_pretty(&slices[0])
                } else {
                    serde_json::to_string_pretty(&slices)
                };
                match out {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                let n = slices.len().max(1);
                let per_budget = args.budget.map(|b| b / n);
                let rendered: Vec<String> = slices
                    .iter()
                    .map(|s| match per_budget {
                        Some(b) => s.render_markdown_budgeted(b, args.numbered),
                        None => s.render_markdown(args.numbered),
                    })
                    .collect();
                print!("{}", rendered.join("\n---\n"));
            }
            ExitCode::SUCCESS
        }
        Commands::Find(args) => {
            let root = find_workspace_root(None, &current_dir);
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

            if cli.json {
                match serde_json::to_string_pretty(&res) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", res.to_markdown());
            }
            ExitCode::SUCCESS
        }
        Commands::Up(args) => {
            let (graph, coord) = match parse_and_prepare(&args.target, &current_dir) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let (callers, value_uses) = graph.upstream(&coord);

            if cli.json {
                let json_callers: Vec<_> = callers.iter().map(|s| s.coordinate()).collect();
                let json_uses: Vec<_> = value_uses.iter().map(|s| s.coordinate()).collect();
                println!(
                    "{}",
                    json!({ "target": coord.to_string(), "callers": json_callers, "value_uses": json_uses })
                );
            } else {
                print!(
                    "{}",
                    mimori::graph::format_upstream(&graph, &coord, &callers, &value_uses)
                );
            }
            ExitCode::SUCCESS
        }
        Commands::Down(args) => {
            let (graph, coord) = match parse_and_prepare(&args.target, &current_dir) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let callees = graph.callees(&coord);

            if cli.json {
                let json_callees: Vec<_> = callees.iter().map(|s| s.coordinate()).collect();
                println!(
                    "{}",
                    json!({ "target": coord.to_string(), "callees": json_callees })
                );
            } else {
                print!(
                    "{}",
                    mimori::graph::format_downstream(&graph, &coord, &callees)
                );
            }
            ExitCode::SUCCESS
        }
        Commands::Uses(args) => {
            let (graph, coord) = match parse_and_prepare(&args.target, &current_dir) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let mentioners = graph.mentioners(&coord);

            if cli.json {
                let json_mentioners: Vec<_> = mentioners.iter().map(|s| s.coordinate()).collect();
                println!(
                    "{}",
                    json!({ "target": coord.to_string(), "mentioners": json_mentioners })
                );
            } else {
                print!(
                    "{}",
                    mimori::graph::format_uses(&graph, &coord, &mentioners)
                );
            }
            ExitCode::SUCCESS
        }
        Commands::Missing(args) => {
            let root = find_workspace_root(None, &current_dir);
            match mimori::graph::missing::find_missing(
                &root,
                args.scope.as_deref(),
                &args.pattern,
                args.defines.as_deref(),
            ) {
                Ok(missing) => {
                    if cli.json {
                        println!(
                            "{}",
                            json!({
                                "pattern": args.pattern,
                                "scope": args.scope,
                                "defines": args.defines,
                                "missing": missing,
                            })
                        );
                    } else {
                        let where_str = args.scope.as_deref().unwrap_or(".");
                        println!(
                            "### Missing `{}` under `{}` ({} files lack it)\n",
                            args.pattern,
                            where_str,
                            missing.len()
                        );
                        if missing.is_empty() {
                            println!("No files missing the pattern.");
                        } else {
                            for f in missing {
                                println!("- 📭 `{}`", f);
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Map(args) => {
            let root = find_workspace_root(None, &current_dir);
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
                args.scope.as_deref(),
                args.focus.as_deref(),
                args.limit,
            );

            if cli.json {
                match serde_json::to_string_pretty(&map_result) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", map_result.to_markdown());
            }
            ExitCode::SUCCESS
        }
        Commands::Diff(args) => {
            if let Err(e) = mimori::graph::diff::validate_diff_base(args.base.as_deref()) {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }
            let root = find_workspace_root(None, &current_dir);
            if mimori::workspace::walker::is_disallowed_workspace_root(&root) {
                eprintln!(
                    "Error: Cannot access home or system directory '{}' as workspace",
                    root.display()
                );
                return ExitCode::FAILURE;
            }
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
            if cli.json {
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", report.to_markdown());
            }
            ExitCode::SUCCESS
        }
        Commands::Blast(args) => {
            let mut cached_graph: Option<(std::path::PathBuf, SymbolGraph)> = None;
            let mut cached_sinks: Option<(std::path::PathBuf, Vec<String>)> = None;
            let sinks = parse_sink_list(args.with_sinks.as_deref());
            let mut json_items = Vec::with_capacity(args.targets.len());
            let mut md_blocks = Vec::with_capacity(args.targets.len());

            for raw_target in &args.targets {
                let (root, graph, coord) =
                    match prepare_cached(raw_target, &current_dir, &mut cached_graph) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            return ExitCode::FAILURE;
                        }
                    };
                let blast_res = if args.down {
                    mimori::graph::blast::calculate_downstream_blast(graph, &coord, args.depth)
                } else {
                    mimori::graph::blast::calculate_blast_radius(graph, &coord, args.depth)
                };
                let blast_res = match blast_res {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return ExitCode::FAILURE;
                    }
                };
                let sink_hits: &[String] = if sinks.is_empty() {
                    &[]
                } else {
                    let need_sweep = cached_sinks
                        .as_ref()
                        .map(|(r, _)| r != &root)
                        .unwrap_or(true);
                    if need_sweep {
                        let hits = sweep_literal_sinks(&root, graph, &sinks);
                        cached_sinks = Some((root.clone(), hits));
                    }
                    &cached_sinks.as_ref().expect("sinks initialized").1
                };

                if cli.json {
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
                    }
                    json_items.push(v);
                } else {
                    md_blocks.push(render_cli_blast_markdown(
                        &blast_res, graph, &coord, args.down, &sinks, sink_hits,
                    ));
                }
            }

            if cli.json {
                let out = if json_items.len() == 1 {
                    serde_json::to_string_pretty(&json_items[0])
                } else {
                    serde_json::to_string_pretty(&json_items)
                };
                match out {
                    Ok(j) => println!("{}", j),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", md_blocks.join("\n---\n"));
            }
            ExitCode::SUCCESS
        }
        Commands::Doctor(args) => {
            let root = find_workspace_root(None, &current_dir);
            let graph = match get_or_sync_graph(&root) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            let res = mimori::graph::doctor::run_doctor(&graph, args.limit);
            if cli.json {
                match serde_json::to_string_pretty(&res) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", res.to_markdown());
            }
            ExitCode::SUCCESS
        }
        Commands::Clean(args) => {
            let root = find_workspace_root(None, &current_dir);
            match clean_cache(&root, args.all) {
                Ok(_) => {
                    if cli.json {
                        println!("{}", json!({ "cleaned": true, "all": args.all }));
                    } else {
                        println!("Cleaned .mimori cache.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error cleaning cache: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Init(args) => {
            let root = &current_dir;
            if mimori::workspace::walker::is_disallowed_workspace_root(root) {
                eprintln!(
                    "INIT_FAIL: Cannot initialize mimori in home or system directory '{}'; exit 1.",
                    root.display()
                );
                return ExitCode::FAILURE;
            }
            let mimori_dir = root.join(".mimori");
            if fs::symlink_metadata(&mimori_dir).is_ok_and(|m| m.file_type().is_symlink()) {
                eprintln!(
                    "INIT_FAIL: Refusing to follow symlink '{}'; exit 1.",
                    mimori_dir.display()
                );
                return ExitCode::FAILURE;
            }
            if let Err(e) = fs::create_dir_all(&mimori_dir) {
                eprintln!(
                    "INIT_FAIL: create dir {}: {}; exit 1.",
                    mimori_dir.display(),
                    e
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

            let (mem_created, dec_created) = match MemoryLedger::scaffold(root, args.force) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("INIT_FAIL: scaffold .agents: {}; exit 1.", e);
                    return ExitCode::FAILURE;
                }
            };

            let agents_status = if mem_created || dec_created {
                "initialized"
            } else {
                "verified"
            };

            if cli.json {
                println!(
                    "{}",
                    json!({
                        "initialized": true,
                        "mimori_cache": mimori_dir.display().to_string(),
                        "gitignore": gitignore_status,
                        "agents": agents_status,
                    })
                );
            } else {
                println!(
                    "INIT: .mimori cache ready; .gitignore {}; .agents/ {} (memory.md, decisions.md); exit 0.",
                    gitignore_status, agents_status
                );
            }
            ExitCode::SUCCESS
        }
        Commands::Memory(args) => match args.command {
            Some(MemoryCommand::Lint) | Some(MemoryCommand::Check) => {
                let ledger = match MemoryLedger::load(&find_workspace_root(None, &current_dir)) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("MEM_LINT_FAIL: load .agents/memory.md: {}; exit 1.", e);
                        return ExitCode::FAILURE;
                    }
                };
                let report = ledger.lint();
                if cli.json {
                    match serde_json::to_string_pretty(&report) {
                        Ok(j) => println!("{}", j),
                        Err(e) => {
                            eprintln!("MEM_ERR: serialize JSON: {}; exit 1.", e);
                            return ExitCode::FAILURE;
                        }
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
            Some(MemoryCommand::Resolve(res_args)) => {
                let mut ledger = match MemoryLedger::load(&find_workspace_root(None, &current_dir))
                {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("MEM_RESOLVE_FAIL: load .agents/memory.md: {}; exit 1.", e);
                        return ExitCode::FAILURE;
                    }
                };
                match ledger.resolve(&res_args.pattern) {
                    Ok(deleted) => {
                        if cli.json {
                            println!(
                                "{}",
                                json!({
                                    "deleted": deleted,
                                    "pattern": res_args.pattern,
                                    "remaining_debt": ledger.raw_debt_lines.len(),
                                    "ceiling": 30
                                })
                            );
                        } else {
                            println!(
                                    "MEM_RESOLVE: deleted {} lines matching '{}'; debt: {}/30 lines; exit 0.",
                                    deleted, res_args.pattern, ledger.raw_debt_lines.len()
                                );
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("MEM_RESOLVE_FAIL: {}; exit 1.", e);
                        ExitCode::FAILURE
                    }
                }
            }
            Some(MemoryCommand::Show(show_args)) => {
                let ledger = match MemoryLedger::load(&find_workspace_root(None, &current_dir)) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("MEM_SHOW_FAIL: load .agents/memory.md: {}; exit 1.", e);
                        return ExitCode::FAILURE;
                    }
                };
                let sec = show_args.section.as_deref().or(args.section.as_deref());
                let budget = show_args.budget.or(args.budget);
                if cli.json {
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
                            "vocab_gotchas": ledger.vocab_gotchas,
                        })
                    };
                    println!("{}", serde_json::to_string_pretty(&val).unwrap());
                    ExitCode::SUCCESS
                } else if let Some(s) = sec {
                    match ledger.get_section(s) {
                        Some(content) => {
                            print_budgeted(&content, budget);
                            ExitCode::SUCCESS
                        }
                        None => {
                            println!("MEM_EMPTY: section '{}' not found; exit 0.", s);
                            ExitCode::SUCCESS
                        }
                    }
                } else {
                    print_budgeted(&ledger.raw_content, budget);
                    ExitCode::SUCCESS
                }
            }
            None => {
                let ledger = match MemoryLedger::load(&find_workspace_root(None, &current_dir)) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("MEM_SHOW_FAIL: load .agents/memory.md: {}; exit 1.", e);
                        return ExitCode::FAILURE;
                    }
                };
                if cli.json {
                    let val = if let Some(s) = args.section.as_deref() {
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
                            "vocab_gotchas": ledger.vocab_gotchas,
                        })
                    };
                    println!("{}", serde_json::to_string_pretty(&val).unwrap());
                    ExitCode::SUCCESS
                } else if let Some(s) = args.section.as_deref() {
                    match ledger.get_section(s) {
                        Some(content) => {
                            print_budgeted(&content, args.budget);
                            ExitCode::SUCCESS
                        }
                        None => {
                            println!("MEM_EMPTY: section '{}' not found; exit 0.", s);
                            ExitCode::SUCCESS
                        }
                    }
                } else {
                    print_budgeted(&ledger.raw_content, args.budget);
                    ExitCode::SUCCESS
                }
            }
        },
        Commands::Debt(args) => match args.command {
            Some(DebtCommand::Check) => {
                let (passed, output) = check_debt(&find_workspace_root(None, &current_dir));
                if cli.json {
                    println!("{}", json!({ "passed": passed, "output": output }));
                } else {
                    println!("{}", output);
                }
                if passed {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Some(DebtCommand::Sync) => match sync_debt(&find_workspace_root(None, &current_dir)) {
                Ok((in_code, manual, synced, output)) => {
                    if cli.json {
                        println!(
                            "{}",
                            json!({
                                "in_code": in_code,
                                "manual": manual,
                                "synced": synced,
                                "ceiling": 30
                            })
                        );
                    } else {
                        println!("{}", output);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("DEBT_SYNC_FAIL: {}; exit 1.", e);
                    ExitCode::FAILURE
                }
            },
            Some(DebtCommand::List(list_args)) => {
                let scope = list_args.scope.as_deref().or(args.scope.as_deref());
                let (markers, output) = list_debt(&find_workspace_root(None, &current_dir), scope);
                if cli.json {
                    match serde_json::to_string_pretty(&markers) {
                        Ok(j) => println!("{}", j),
                        Err(e) => {
                            eprintln!("DEBT_ERR: serialize JSON: {}; exit 1.", e);
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!("{}", output);
                }
                ExitCode::SUCCESS
            }
            None => {
                let (markers, output) = list_debt(
                    &find_workspace_root(None, &current_dir),
                    args.scope.as_deref(),
                );
                if cli.json {
                    match serde_json::to_string_pretty(&markers) {
                        Ok(j) => println!("{}", j),
                        Err(e) => {
                            eprintln!("DEBT_ERR: serialize JSON: {}; exit 1.", e);
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!("{}", output);
                }
                ExitCode::SUCCESS
            }
        },
        Commands::Dump(args) => {
            match generate_dump(
                &find_workspace_root(None, &current_dir),
                args.budget,
                args.focus.as_deref(),
            ) {
                Ok(dump) => {
                    if cli.json {
                        match serde_json::to_string_pretty(&dump) {
                            Ok(j) => println!("{}", j),
                            Err(e) => {
                                eprintln!("DUMP_ERR: serialize JSON: {}; exit 1.", e);
                                return ExitCode::FAILURE;
                            }
                        }
                    } else {
                        print!("{}", dump.markdown);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("DUMP_FAIL: {}; exit 1.", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Mcp(args) => match mimori::mcp::run_mcp_server(args.workspace) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error running MCP server: {}", e);
                ExitCode::FAILURE
            }
        },
    }
}

/// Parse a raw target, locate the workspace root from it, load the index, and
/// return the coordinate normalized onto the paths the index stores.
fn parse_and_prepare(raw: &str, cwd: &Path) -> Result<(SymbolGraph, Coordinate)> {
    prepare(Coordinate::parse(raw)?, cwd)
}

fn validate_coord_workspace(coord: &Coordinate, cwd: &Path) -> Result<std::path::PathBuf> {
    let root = find_workspace_root(coord.absolute_parent().as_deref(), cwd);
    if mimori::workspace::walker::is_disallowed_workspace_root(&root) {
        bail!(
            "Cannot access home or system directory '{}' as workspace",
            root.display()
        );
    }
    if let Some(file) = coord.file() {
        if file.is_absolute() {
            mimori::workspace::walker::confine_to_workspace(&root, file)?;
        } else if file
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            let full = root.join(file);
            mimori::workspace::walker::confine_to_workspace(&root, &full)?;
        } else {
            for candidate in [cwd.join(file), root.join(file)] {
                if fs::symlink_metadata(&candidate).is_ok() {
                    mimori::workspace::walker::confine_to_workspace(&root, &candidate)?;
                    break;
                }
            }
        }
    }
    Ok(root)
}

fn prepare(coord: Coordinate, cwd: &Path) -> Result<(SymbolGraph, Coordinate)> {
    let root = validate_coord_workspace(&coord, cwd)?;
    let graph = get_or_sync_graph(&root)?;
    Ok((graph, coord.normalize_against(&root)))
}

fn prepare_cached<'a>(
    raw: &str,
    cwd: &Path,
    cache: &'a mut Option<(std::path::PathBuf, SymbolGraph)>,
) -> Result<(std::path::PathBuf, &'a SymbolGraph, Coordinate)> {
    let coord = Coordinate::parse(raw)?;
    let root = validate_coord_workspace(&coord, cwd)?;
    let need_load = cache.as_ref().map(|(r, _)| r != &root).unwrap_or(true);
    if need_load {
        let graph =
            get_or_sync_graph(&root).map_err(|e| anyhow::anyhow!("syncing workspace: {}", e))?;
        *cache = Some((root.clone(), graph));
    }
    let (_, ref graph) = cache.as_ref().expect("graph initialized");
    Ok((root.clone(), graph, coord.normalize_against(&root)))
}

fn build_cli_slice(
    raw: &str,
    cwd: &Path,
    follow_local: bool,
    with_imports: bool,
    cache: &mut Option<(std::path::PathBuf, SymbolGraph)>,
) -> Result<mimori::model::SliceResult> {
    let coord = Coordinate::parse(raw)?;
    if let Coordinate::Lines { file, start, end } = &coord {
        let root = find_workspace_root(coord.absolute_parent().as_deref(), cwd);
        if mimori::workspace::walker::is_disallowed_workspace_root(&root) {
            bail!(
                "Cannot access home or system directory '{}' as workspace",
                root.display()
            );
        }
        let full = if file.is_absolute() {
            file.to_path_buf()
        } else {
            let from_cwd = cwd.join(file);
            if fs::symlink_metadata(&from_cwd).is_ok() {
                from_cwd
            } else {
                root.join(file)
            }
        };
        let target_path = mimori::workspace::walker::confine_to_workspace(&root, &full)?;
        return slice_line_coordinate(&target_path, *start, *end, with_imports);
    }
    let (_, graph, norm_coord) = prepare_cached(raw, cwd, cache)?;
    graph.build_slice(&norm_coord, follow_local, with_imports)
}

fn render_cli_blast_markdown(
    blast_res: &mimori::graph::blast::BlastResult,
    graph: &SymbolGraph,
    coord: &Coordinate,
    down: bool,
    sinks: &[String],
    sink_hits: &[String],
) -> String {
    let mut out = blast_res.to_markdown();
    if blast_res.affected.is_empty() && blast_res.value_uses.is_empty() && !down {
        if let Some(hint) = zero_caller_hint(graph, coord) {
            out.push_str(&hint);
            out.push('\n');
        }
    }
    if !sinks.is_empty() {
        out.push_str(&format_sink_hits(sinks, sink_hits));
    } else {
        out.push_str("\nTip: blast follows call edges plus labeled value-use rows — raw literals (console.*, logPath, send*Notifications, getDetailedMessage) need a sweep: `mimori blast <target> --with-sinks console.,logPath,sendNotification,getDetailedMessage` or `rg -n \"console\\.|logPath|send.*Notif|getDetailedMessage\"`.\n");
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
                    println!("… [truncated to fit token budget]");
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
