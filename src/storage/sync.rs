use crate::graph::SymbolGraph;
use crate::parser::parse_file;
use crate::storage::db::Database;
use anyhow::Result;
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub fn get_or_sync_graph(root: &Path) -> Result<SymbolGraph> {
    let mimori_dir = root.join(".mimori");
    if !mimori_dir.exists() {
        let _ = fs::create_dir_all(&mimori_dir);
    }

    let db_path = mimori_dir.join("index.db");
    let mut db = Database::open_or_create(&db_path)?;

    let disk_files = discover_workspace_files(root);
    let db_files = db.get_file_records()?;

    let mut modified = false;
    let mut disk_paths_set = HashSet::new();

    for (rel_path, full_path, mtime) in &disk_files {
        let rel_str = rel_path.to_string_lossy().to_string();
        disk_paths_set.insert(rel_str.clone());

        let needs_reparse =
            !matches!(db_files.get(&rel_str), Some((_id, db_mtime, _hash)) if *db_mtime == *mtime);

        if needs_reparse {
            modified = true;
            if let Ok(content) = fs::read_to_string(full_path) {
                if let Ok(mut syms) = parse_file(full_path, &content) {
                    for s in &mut syms {
                        s.file = rel_str.clone();
                    }
                    let hash = format!("{:x}", fnv1a_hash(content.as_bytes()));
                    db.save_file_and_symbols(&rel_str, *mtime, &hash, &syms)?;
                }
            }
        }
    }

    // Check for deleted files
    for (db_path_str, (file_id, _, _)) in db_files {
        if !disk_paths_set.contains(&db_path_str) {
            modified = true;
            let _ = db.delete_file_by_id(file_id);
        }
    }

    let symbols = db.load_all_symbols()?;
    let graph = SymbolGraph::new(symbols);

    if modified {
        let _ = db.update_centralities(&graph.symbols);
    }

    Ok(graph)
}

pub fn clean_cache(root: &Path, all: bool) -> Result<()> {
    let mimori_dir = root.join(".mimori");
    if !mimori_dir.exists() {
        return Ok(());
    }

    let files_to_remove = ["index.db", "index.db-wal", "index.db-shm", "cache.bin"];
    for f in &files_to_remove {
        let p = mimori_dir.join(f);
        if p.exists() {
            let _ = fs::remove_file(p);
        }
    }

    if all {
        let cache_dir = mimori_dir.join(".cache");
        if cache_dir.exists() {
            let _ = fs::remove_dir_all(cache_dir);
        }
    }

    Ok(())
}

fn discover_workspace_files(root: &Path) -> Vec<(PathBuf, PathBuf, i64)> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".mimoriignore");

    let mut list = Vec::new();

    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if path.is_file() {
            let path_str = path.to_string_lossy();
            if is_ignored_path(&path_str) {
                continue;
            }

            if has_supported_extension(path) {
                let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);

                list.push((rel, path.to_path_buf(), mtime));
            }
        }
    }

    list
}

fn has_supported_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs")
            | Some("ts")
            | Some("tsx")
            | Some("js")
            | Some("jsx")
            | Some("py")
            | Some("go")
    )
}

fn is_ignored_path(path_str: &str) -> bool {
    let ignores = [
        "/target/",
        "/node_modules/",
        "/.git/",
        "/dist/",
        "/build/",
        "/vendor/",
        "/.mimori/",
        "target/",
        "node_modules/",
        ".git/",
        ".mimori/",
    ];

    ignores.iter().any(|&ign| path_str.contains(ign))
}

fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
