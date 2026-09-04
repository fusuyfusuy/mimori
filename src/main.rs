use anyhow::Result;
use clap::Parser;
use mimori::cli::{Cli, Commands};
use mimori::graph::map::generate_map;
use mimori::graph::{slice_line_coordinate, SymbolGraph};
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
                slice_line_coordinate(file, *start, *end, args.with_imports)
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
                        match serde_json::to_string_pretty(&slice) {
                            Ok(json) => println!("{}", json),
                            Err(e) => {
                                eprintln!("Error serializing JSON: {}", e);
                                return ExitCode::FAILURE;
                            }
                        }
                    } else {
                        print!("{}", slice.to_markdown());
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

            let callers = graph.callers(&coord);

            if cli.json {
                let json_callers: Vec<_> = callers.iter().map(|s| s.coordinate()).collect();
                println!("{}", json!({ "target": coord.to_string(), "callers": json_callers }));
            } else {
                println!("### Upstream Callers: `{}` ({} callers)\n", coord, callers.len());
                if callers.is_empty() {
                    println!("No upstream callers found.");
                } else {
                    for c in callers {
                        println!("- 🔺 **`{}`** ({}) → `{}`", c.name, c.kind.as_str(), c.coordinate());
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
                println!("{}", json!({ "target": coord.to_string(), "callees": json_callees }));
            } else {
                println!("### Downstream Callees: `{}` ({} callees)\n", coord, callees.len());
                if callees.is_empty() {
                    println!("No downstream callees found.");
                } else {
                    for c in callees {
                        println!("- 🔻 **`{}`** ({}) → `{}`", c.name, c.kind.as_str(), c.coordinate());
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Commands::Map(args) => {
            let mut graph = match get_or_sync_graph(&current_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            if let Err(e) = personalize(&mut graph, args.focus.as_deref(), args.seed.as_deref(), &current_dir) {
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

            match mimori::graph::blast::calculate_blast_radius(&graph, &coord, args.depth) {
                Ok(blast_res) => {
                    if cli.json {
                        match serde_json::to_string_pretty(&blast_res) {
                            Ok(json) => println!("{}", json),
                            Err(e) => {
                                eprintln!("Error serializing JSON: {}", e);
                                return ExitCode::FAILURE;
                            }
                        }
                    } else {
                        print!("{}", blast_res.to_markdown());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Clean(args) => {
            match clean_cache(&current_dir, args.all) {
                Ok(_) => {
                    println!("Cleaned .mimori cache.");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error cleaning cache: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Init => {
            let mimori_dir = current_dir.join(".mimori");
            let _ = fs::create_dir_all(&mimori_dir);
            println!("Initialized .mimori workspace memory");
            ExitCode::SUCCESS
        }
        Commands::Log(args) => {
            let files: Vec<String> = match args.files {
                Some(f) => f
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                None => Vec::new(),
            };

            let record =
                mimori::workspace::ActivityRecord::new(args.action.clone(), args.summary, files);

            if let Err(e) = mimori::workspace::append_activity(&current_dir, &record) {
                eprintln!("Error logging activity: {}", e);
                return ExitCode::FAILURE;
            }

            if cli.json {
                match serde_json::to_string_pretty(&record) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                println!("Logged action: {} - {}", record.action, record.summary);
            }
            ExitCode::SUCCESS
        }
        Commands::Dump(args) => {
            let mut graph = match get_or_sync_graph(&current_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            if let Err(e) = personalize(&mut graph, None, args.seed.as_deref(), &current_dir) {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }

            let map_result = generate_map(&graph, args.scope.as_deref(), None, args.limit);
            let recent_activity =
                mimori::workspace::read_recent_activity(&current_dir, 10).unwrap_or_default();

            if cli.json {
                let dump_json = json!({
                    "map": map_result,
                    "recent_activity": recent_activity,
                });
                match serde_json::to_string_pretty(&dump_json) {
                    Ok(json) => println!("{}", json),
                    Err(e) => {
                        eprintln!("Error serializing JSON: {}", e);
                        return ExitCode::FAILURE;
                    }
                }
                return ExitCode::SUCCESS;
            }

            let mut md = String::new();
            if !recent_activity.is_empty() {
                md.push_str("## 📜 Recent Activity\n\n");
                for act in &recent_activity {
                    let files_str = if act.files.is_empty() {
                        String::new()
                    } else {
                        format!(" (`{}`)", act.files.join(", "))
                    };
                    md.push_str(&format!(
                        "- [{}] **`{}`**: {}{}\n",
                        act.timestamp, act.action, act.summary, files_str
                    ));
                }
                md.push_str("\n---\n\n");
            }
            md.push_str(&map_result.to_markdown());

            if args.file {
                let cache_dir = current_dir.join(".mimori").join(".cache");
                let _ = fs::create_dir_all(&cache_dir);
                let out_file = cache_dir.join("context.md");
                if let Err(e) = fs::write(&out_file, &md) {
                    eprintln!("Error writing context dump: {}", e);
                    return ExitCode::FAILURE;
                }
                println!("Context snapshot written to {}", out_file.display());
            } else {
                print!("{}", md);
            }
            ExitCode::SUCCESS
        }
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
    let graph = get_or_sync_graph(&root)?;
    Ok((graph, coord.normalize_against(&root)))
}
