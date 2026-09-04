use clap::Parser;
use mimori::cli::{Cli, Commands};
use mimori::graph::map::generate_map;
use mimori::storage::{clean_cache, get_or_sync_graph};
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
            let root_dir = get_target_root(&args.target, &current_dir);
            let graph = match get_or_sync_graph(&root_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            match graph.build_slice(&args.target, args.follow_local, args.with_imports) {
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
            let root_dir = get_target_root(&args.target, &current_dir);
            let graph = match get_or_sync_graph(&root_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let callers = graph.callers(&args.target);

            if cli.json {
                let json_callers: Vec<_> = callers.iter().map(|s| s.coordinate()).collect();
                println!("{}", json!({ "target": args.target, "callers": json_callers }));
            } else {
                println!("### Upstream Callers: `{}` ({} callers)\n", args.target, callers.len());
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
            let root_dir = get_target_root(&args.target, &current_dir);
            let graph = match get_or_sync_graph(&root_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let callees = graph.callees(&args.target);

            if cli.json {
                let json_callees: Vec<_> = callees.iter().map(|s| s.coordinate()).collect();
                println!("{}", json!({ "target": args.target, "callees": json_callees }));
            } else {
                println!("### Downstream Callees: `{}` ({} callees)\n", args.target, callees.len());
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

            if let Some(focus_target) = &args.focus {
                graph.compute_personalized_pagerank(focus_target);
            }

            let map_result = generate_map(&graph, args.scope.as_deref(), args.focus.as_deref());

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
            let root_dir = get_target_root(&args.target, &current_dir);
            let graph = match get_or_sync_graph(&root_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            match mimori::graph::blast::calculate_blast_radius(&graph, &args.target, args.depth) {
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

            let summary = if args.summary.len() > 160 {
                format!("{}...", &args.summary[..157])
            } else {
                args.summary.clone()
            };

            let record = mimori::workspace::ActivityRecord {
                timestamp: mimori::workspace::current_utc_timestamp(),
                action: args.action.clone(),
                summary,
                files,
            };

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
            let graph = match get_or_sync_graph(&current_dir) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error syncing workspace: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            let map_result = generate_map(&graph, None, None);
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

fn get_target_root(target: &str, default_dir: &Path) -> std::path::PathBuf {
    if let Some((target_file, _)) = target.split_once(':') {
        let p = Path::new(target_file);
        if p.is_absolute() {
            if let Some(parent) = p.parent() {
                return parent.to_path_buf();
            }
        }
    }
    default_dir.to_path_buf()
}
