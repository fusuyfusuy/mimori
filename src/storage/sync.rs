use crate::graph::SymbolGraph;
use crate::parser::parse_file;
use crate::storage::db::Database;
use crate::workspace::walker::scan_workspace;
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub fn get_or_sync_graph(root: &Path) -> Result<SymbolGraph> {
    let mimori_dir = root.join(".mimori");
    if !mimori_dir.exists() {
        fs::create_dir_all(&mimori_dir)?;
    }

    let db_path = mimori_dir.join("index.db");
    let mut db = Database::open_or_create(&db_path)?;

    let scans = scan_workspace(root);
    let db_files = db.get_file_records()?;

    let mut disk_paths = HashSet::new();
    let mut stale = Vec::new();

    for scan in &scans {
        let rel = scan.rel.to_string_lossy().to_string();
        disk_paths.insert(rel.clone());

        // Invalidate on content, not on timestamps. mtime is recorded but never
        // trusted: preserving it while changing content is routine (cp -p,
        // touch -r, rsync -t) and used to leave the index silently wrong.
        let cached = db_files.get(&rel);
        if !matches!(cached, Some((_, _, db_hash)) if *db_hash == scan.hash) {
            stale.push((rel, scan));
        }
    }

    // Parse in parallel; write serially, since the connection is not Sync.
    let parsed: Vec<(String, i64, String, Option<Vec<_>>)> = stale
        .par_iter()
        .map(|(rel, scan)| {
            let full = root.join(&scan.rel);
            let symbols = parse_file(&full, &scan.content).ok().map(|mut syms| {
                for s in &mut syms {
                    s.file = rel.clone();
                }
                syms
            });
            (rel.clone(), scan.mtime, scan.hash.clone(), symbols)
        })
        .collect();

    let mut unparsed = Vec::new();

    for (rel, mtime, hash, symbols) in parsed {
        // A file that fails to parse is still recorded, with zero symbols. Not
        // recording it left `needs_reparse` true forever, so every subsequent
        // command re-read and re-parsed it, invisibly.
        let syms = symbols.unwrap_or_else(|| {
            unparsed.push(rel.clone());
            Vec::new()
        });
        db.save_file_and_symbols(&rel, mtime, &hash, &syms)?;
    }

    for (db_path_str, (file_id, _, _)) in db_files {
        if !disk_paths.contains(&db_path_str) {
            db.delete_file_by_id(file_id)?;
        }
    }

    if !unparsed.is_empty() {
        eprintln!(
            "mimori: {} file(s) could not be parsed and are indexed as empty: {}",
            unparsed.len(),
            preview(&unparsed)
        );
    }

    let symbols = db.load_all_symbols()?;
    Ok(SymbolGraph::new(symbols))
}

fn preview(paths: &[String]) -> String {
    const MAX: usize = 3;
    if paths.len() <= MAX {
        return paths.join(", ");
    }
    format!("{}, and {} more", paths[..MAX].join(", "), paths.len() - MAX)
}

pub fn clean_cache(root: &Path, all: bool) -> Result<()> {
    let mimori_dir = root.join(".mimori");
    if !mimori_dir.exists() {
        return Ok(());
    }

    for f in ["index.db", "index.db-wal", "index.db-shm", "cache.bin"] {
        let p = mimori_dir.join(f);
        if p.exists() {
            fs::remove_file(p)?;
        }
    }

    if all {
        let cache_dir = mimori_dir.join(".cache");
        if cache_dir.exists() {
            fs::remove_dir_all(cache_dir)?;
        }
    }

    Ok(())
}
