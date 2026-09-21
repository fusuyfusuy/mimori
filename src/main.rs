use anyhow::{bail, Result};
use clap::Parser;
use mimori::cli::{Cli, Commands, DebtCommand, MemoryCommand};
use mimori::graph::map::generate_map;
use mimori::graph::{slice_line_coordinate, SymbolGraph, COUNT_LEGEND};
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
            let coord = match Coordinate::parse(&args.target) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            // A line range is read straight off disk; it needs no index.
            let built = if let Coordinate::Lines { file, start, end } = &coord {
                let root = find_workspace_root(coord.absolute_parent().as_deref(), &current_dir);
                if mimori::workspace::walker::is_system_directory(&root) {
                    eprintln!(
                        "Error: Cannot access system directory '{}' as workspace",
                        root.display()
                    );
                    return ExitCode::FAILURE;
                }
                let full = if file.is_absolute() {
                    file.to_path_buf()
                } else {
                    current_dir.join(file)
                };
                let target_path =
                    match mimori::workspace::walker::confine_to_workspace(&root, &full) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            return ExitCode::FAILURE;
                        }
                    };
                slice_line_coordinate(&target_path, *start, *end, args.with_imports)
            } else {
                match prepare(coord, &current_dir) {
                    Ok((graph, coord)) => {
                        graph.build_slice(&coord, args.follow_local, args.with_imports)
                    }
                    Err(e) => {
                        eprintln!("Error syncing workspace: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            };

            match built {
                Ok(slice) => {
                    if cli.json {
                        if args.budget.is_some() {
                            eprintln!("Warning: --budget only affects markdown output; ignoring for --json.");
                        }
                        match serde_json::to_string_pretty(&slice) {
                            Ok(json) => println!("{}", json),
                            Err(e) => {
                                eprintln!("Error serializing JSON: {}", e);
                                return ExitCode::FAILURE;
                            }
                        }
                    } else if let Some(budget) = args.budget {
                        print!("{}", slice.render_markdown_budgeted(budget, args.numbered));
                    } else {
                        print!("{}", slice.render_markdown(args.numbered));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Find(args) => {
            let res = match mimori::workspace::execute_find(
                &current_dir,
                &args.pattern,
                args.symbols_only,
                args.files_only,
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
                println!(
                    "### Upstream Callers: `{}` ({} callers)\n",
                    coord,
                    callers.len()
                );
                println!("{}\n", COUNT_LEGEND);
                if callers.is_empty() && value_uses.is_empty() {
                    println!("No upstream callers found.");
                    // P2: zero-caller UX hint. A class with no callers almost
                    // always means "look at construction sites instead".
                    if let Some(hint) = zero_caller_hint(&graph, &coord) {
                        println!("{}", hint);
                    }
                } else {
                    for c in callers {
                        println!(
                            "- 🔺 **`{}`** ({}) → `{}`",
                            c.name,
                            c.kind.as_str(),
                            c.coordinate()
                        );
                    }
                    if !value_uses.is_empty() {
                        println!("\n### 📎 Value Uses (mentions — weakest tier, not call edges)\n");
                        for m in value_uses {
                            println!(
                                "- 📎 **`{}`** ({}) → `{}` [Value Use]",
                                m.name,
                                m.kind.as_str(),
                                m.coordinate()
                            );
                        }
                    }
                }
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
                println!(
                    "### Downstream Callees: `{}` ({} callees)\n",
                    coord,
                    callees.len()
                );
                println!("{}\n", COUNT_LEGEND);
                if callees.is_empty() {
                    println!("No downstream callees found.");
                } else {
                    for c in callees {
                        println!(
                            "- 🔻 **`{}`** ({}) → `{}`",
                            c.name,
                            c.kind.as_str(),
                            c.coordinate()
                        );
                    }
                }
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
                println!(
                    "### Mentioners: `{}` ({} mentioners, non-call uses)\n",
                    coord,
                    mentioners.len()
                );
                println!("{}\n", COUNT_LEGEND);
                if mentioners.is_empty() {
                    println!("No mentioners found.");
                } else {
                    for m in mentioners {
                        println!(
                            "- 📎 **`{}`** ({}) → `{}`",
                            m.name,
                            m.kind.as_str(),
                            m.coordinate()
                        );
                    }
                }
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

            if let Err(e) = personalize(
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
        Commands::Blast(args) => {
            let (graph, coord) = match parse_and_prepare(&args.target, &current_dir) {
                Ok(pair) => pair,
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
            match blast_res {
                Ok(blast_res) => {
                    let sinks = parse_sink_list(args.with_sinks.as_deref());
                    // P3: literal sweep runs against the same indexed files so
                    // one call covers both graph reachability and literal sinks.
                    let sink_hits: Vec<String> = if sinks.is_empty() {
                        Vec::new()
                    } else {
                        let root =
                            find_workspace_root(coord.absolute_parent().as_deref(), &current_dir);
                        sweep_literal_sinks(&root, &graph, &sinks)
                    };
                    if cli.json {
                        match serde_json::to_value(&blast_res) {
                            Ok(mut v) => {
                                if !sinks.is_empty() {
                                    v["sinks"] = json!(sinks);
                                    v["sink_hits"] = json!(sink_hits);
                                }
                                match serde_json::to_string_pretty(&v) {
                                    Ok(json) => println!("{}", json),
                                    Err(e) => {
                                        eprintln!("Error serializing JSON: {}", e);
                                        return ExitCode::FAILURE;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Error serializing JSON: {}", e);
                                return ExitCode::FAILURE;
                            }
                        }
                    } else {
                        print!("{}", blast_res.to_markdown());
                        if blast_res.affected.is_empty()
                            && blast_res.value_uses.is_empty()
                            && !args.down
                        {
                            // P2: same zero-caller detour as `up`.
                            if let Some(hint) = zero_caller_hint(&graph, &coord) {
                                println!("{}", hint);
                            }
                        }
                        if !sinks.is_empty() {
                            print_sink_hits(&sinks, &sink_hits);
                        } else {
                            // P3 recipe: keep the graph-vs-literals boundary but
                            // always leave the next command pastable.
                            println!("\nTip: blast follows call edges plus labeled value-use rows — raw literals (console.*, logPath, send*Notifications, getDetailedMessage) need a sweep: `mimori blast <target> --with-sinks console.,logPath,sendNotification,getDetailedMessage` or `rg -n \"console\\.|logPath|send.*Notif|getDetailedMessage\"`.");
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
            let root = find_workspace_root(None, &current_dir);
            let mimori_dir = root.join(".mimori");
            if let Err(e) = fs::create_dir_all(&mimori_dir) {
                eprintln!(
                    "INIT_FAIL: create dir {}: {}; exit 1.",
                    mimori_dir.display(),
                    e
                );
                return ExitCode::FAILURE;
            }

            let gitignore_path = root.join(".gitignore");
            let gitignore_status = if gitignore_path.exists() {
                let content = fs::read_to_string(&gitignore_path).unwrap_or_default();
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
                    let _ = fs::write(&gitignore_path, updated);
                    "updated"
                }
            } else {
                let _ = fs::write(&gitignore_path, ".mimori/\n");
                "created"
            };

            let (mem_created, dec_created) = match MemoryLedger::scaffold(
                &find_workspace_root(None, &current_dir),
                args.force,
            ) {
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

/// Apply `--focus` (personalized PageRank around a symbol) and/or `--seed`
/// (bias toward symbols matching a term) to the ranking.
fn personalize(
    graph: &mut SymbolGraph,
    focus: Option<&str>,
    seed: Option<&str>,
    cwd: &Path,
) -> Result<()> {
    let mut indices = Vec::new();

    if let Some(target) = focus {
        let coord = Coordinate::parse(target)?.normalize_against(cwd);
        indices.extend(graph.resolve_all(&coord));
    }
    if let Some(term) = seed {
        indices.extend(graph.seed_indices(term));
    }

    indices.sort_unstable();
    indices.dedup();

    if !indices.is_empty() {
        graph.apply_personalization(&indices);
    }
    Ok(())
}

/// Parse a raw target, locate the workspace root from it, load the index, and
/// return the coordinate normalized onto the paths the index stores.
fn parse_and_prepare(raw: &str, cwd: &Path) -> Result<(SymbolGraph, Coordinate)> {
    prepare(Coordinate::parse(raw)?, cwd)
}

fn prepare(coord: Coordinate, cwd: &Path) -> Result<(SymbolGraph, Coordinate)> {
    let root = find_workspace_root(coord.absolute_parent().as_deref(), cwd);
    if mimori::workspace::walker::is_system_directory(&root) {
        bail!(
            "Cannot access system directory '{}' as workspace",
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
        }
    }
    let graph = get_or_sync_graph(&root)?;
    Ok((graph, coord.normalize_against(&root)))
}

/// P2: when `up`/`blast` on a class-like returns zero, point at the
/// constructor and the confirming `rg` before the user detours.
fn zero_caller_hint(graph: &SymbolGraph, coord: &Coordinate) -> Option<String> {
    let name = coord.name()?;
    // Skip targets that already name a member (`X::ctor`) — the hint is for
    // bare classes whose construction sites live under the constructor.
    if name.contains("::") || name.contains('.') {
        return None;
    }
    let has_ctor = ["::constructor", "::new"].iter().any(|sfx| {
        graph
            .name_to_indices
            .contains_key(&format!("{}{}", name, sfx))
    });
    let is_type = graph.resolve_all(coord).iter().any(|&i| {
        matches!(
            graph.symbols[i].kind.as_str(),
            "class" | "struct" | "interface" | "trait"
        )
    });
    if has_ctor || is_type {
        Some(format!(
            "💡 0 callers — if `{0}` is a class, construction sites are `new {0}(...)` edges: try `mimori up {0}::constructor`, and confirm with `rg -n \"new {0}\"`.",
            name
        ))
    } else {
        None
    }
}

fn parse_sink_list(raw: Option<&str>) -> Vec<String> {
    match raw {
        None => Vec::new(),
        Some(s) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
    }
}

/// P3: case-sensitive substring sweep over indexed files, one hit per matching
/// line, capped so a noisy sink (e.g. `console.`) can't flood the context.
fn sweep_literal_sinks(root: &Path, graph: &SymbolGraph, sinks: &[String]) -> Vec<String> {
    const CAP: usize = 100;
    let mut files: Vec<&str> = graph.symbols.iter().map(|s| s.file.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    let mut hits = Vec::new();
    'files: for rel in files {
        let Ok(content) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if sinks.iter().any(|s| line.contains(s)) {
                hits.push(format!("{}:#L{}: {}", rel, idx + 1, line.trim()));
                if hits.len() >= CAP {
                    hits.push(format!(
                        "… capped at {} hits; refine --with-sinks or use rg.",
                        CAP
                    ));
                    break 'files;
                }
            }
        }
    }
    hits
}

fn print_sink_hits(sinks: &[String], hits: &[String]) {
    println!(
        "\n### 🔍 Literal sink hits (`--with-sinks {}`)\n",
        sinks.join(",")
    );
    if hits.is_empty() {
        println!("No literal sink hits.");
    } else {
        for h in hits {
            println!("- `{}`", h);
        }
    }
}

fn print_budgeted(text: &str, budget: Option<usize>) {
    if let Some(b) = budget {
        let max_chars = b * 4;
        if text.len() > max_chars {
            let mut acc = 0;
            for line in text.lines() {
                if acc + line.len() + 1 > max_chars {
                    println!("… [truncated to fit token budget]");
                    break;
                }
                println!("{}", line);
                acc += line.len() + 1;
            }
            return;
        }
    }
    print!("{}", text);
    if !text.ends_with('\n') {
        println!();
    }
}
