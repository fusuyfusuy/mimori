use crate::graph::SymbolGraph;
use crate::parser::parse_file;
use crate::storage::db::Database;
use crate::workspace::walker::{
    discover_workspace, fnv1a_hash, hash_discovered_files, HashedFile, WorkspaceDiscovery,
};
use crate::workspace::AliasSet;
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Single-pass workspace discovery + FNV-1a content hashes, shared between
/// `McpCache` invalidation and `sync_prepared_workspace` so zero files are
/// walked or hashed twice.
pub struct PreparedWorkspace {
    pub discovery: WorkspaceDiscovery,
    pub aliases: AliasSet,
    pub hashed: Vec<HashedFile>,
    pub transient_failed: Vec<String>,
    pub indexed_files: usize,
    pub content_fingerprint: String,
}

pub fn prepare_workspace(root: &Path) -> Result<PreparedWorkspace> {
    if crate::workspace::walker::is_disallowed_workspace_root(root) {
        anyhow::bail!(
            "Refusing to index home or system directory '{}' as workspace root",
            root.display()
        );
    }

    let _p = crate::Phase::start("scan+read+hash");
    let discovery = discover_workspace(root);
    let aliases = AliasSet::collect_with_discovery(root, &discovery);
    let (hashed, transient_failed, indexed_files) = hash_discovered_files(&discovery);
    drop(_p);

    let mut fp_hash: u64 = 0xcbf29ce484222325;
    let mix = |h: u64, bytes: &[u8]| fnv1a_hash(bytes) ^ h.wrapping_mul(0x100000001b3);
    fp_hash = mix(fp_hash, aliases.fingerprint().as_bytes());
    for h in &hashed {
        fp_hash = mix(fp_hash, h.rel.as_bytes());
        fp_hash = mix(fp_hash, h.hash.as_bytes());
    }
    let content_fingerprint = format!("{fp_hash:016x}");

    Ok(PreparedWorkspace {
        discovery,
        aliases,
        hashed,
        transient_failed,
        indexed_files,
        content_fingerprint,
    })
}

pub fn get_or_sync_graph(root: &Path) -> Result<SymbolGraph> {
    let prep = prepare_workspace(root)?;
    sync_prepared_workspace(root, &prep)
}

pub fn sync_prepared_workspace(root: &Path, prep: &PreparedWorkspace) -> Result<SymbolGraph> {
    let mimori_dir = root.join(".mimori");

    if !mimori_dir.exists() {
        fs::create_dir_all(&mimori_dir)?;
    }

    let db_path = mimori_dir.join("index.db");
    let mut db = Database::open_or_create(&db_path)?;

    db.enforce_alias_fingerprint(prep.aliases.fingerprint())?;
    // Read existing DB file records BEFORE comparing against the disk snapshot.
    let db_files = db.get_file_records()?;

    let mut disk_paths: HashSet<&str> =
        HashSet::with_capacity(prep.hashed.len() + prep.transient_failed.len());
    for failed_rel in &prep.transient_failed {
        disk_paths.insert(failed_rel.as_str());
    }

    let mut stale: Vec<&HashedFile> = Vec::new();
    for h in &prep.hashed {
        disk_paths.insert(h.rel.as_str());
        let cached = db_files.get(&h.rel);
        if !matches!(cached, Some((_, _, db_hash)) if *db_hash == h.hash) {
            stale.push(h);
        }
    }

    // Read full source text ONLY for stale/new files, parse in parallel.
    let _p = crate::Phase::start("parse (parallel)");
    let parsed: Vec<(String, i64, String, Option<Vec<_>>)> = stale
        .par_iter()
        .filter_map(|h| {
            let bytes = fs::read(&h.full).ok()?;
            let content = String::from_utf8_lossy(&bytes);
            let symbols = parse_file(&h.full, &content, &prep.aliases)
                .ok()
                .map(|mut syms| {
                    for s in &mut syms {
                        s.file = h.rel.clone();
                    }
                    syms
                });
            Some((h.rel.clone(), h.mtime, h.hash.clone(), symbols))
        })
        .collect();

    drop(_p);
    let _p = crate::Phase::start("db write");
    let mut unparsed = Vec::new();
    let mut batch_updates = Vec::with_capacity(parsed.len());

    for (rel, mtime, hash, symbols) in &parsed {
        let syms: &[crate::model::Symbol] = match symbols {
            Some(syms) => syms.as_slice(),
            None => {
                unparsed.push(rel.clone());
                &[]
            }
        };
        batch_updates.push((rel.as_str(), *mtime, hash.as_str(), syms));
    }

    let mut deleted_file_ids = Vec::new();
    for (db_path_str, (file_id, _, _)) in &db_files {
        if !disk_paths.contains(db_path_str.as_str()) {
            deleted_file_ids.push(*file_id);
        }
    }

    db.save_batch(&batch_updates, &deleted_file_ids)?;
    drop(_p);
    if !unparsed.is_empty() {
        eprintln!(
            "mimori: {} file(s) could not be parsed and are indexed as empty: {}",
            unparsed.len(),
            preview(&unparsed)
        );
    }

    let _p = crate::Phase::start("load_all_symbols");
    let symbols = db.load_all_symbols()?;
    drop(_p);
    let _p = crate::Phase::start("SymbolGraph::new");
    let mut g = SymbolGraph::new(symbols);
    drop(_p);
    g.root = Some(root.to_path_buf());
    g.indexed_files = prep.hashed.iter().map(|h| h.rel.clone()).collect();
    let scan_stats = &prep.discovery.stats;
    g.coverage = Some(crate::graph::CoverageStats {
        indexed_files: prep.indexed_files,
        crawled_files: scan_stats.crawled_files,
        unindexed_exts: scan_stats.unindexed_exts.clone(),
    });
    if scan_stats.crawled_files > 0 && prep.indexed_files * 2 < scan_stats.crawled_files {
        eprintln!(
            "Note: indexed {}/{} files ({}%); unindexed exts: {}. Coverage below 50% — map/doctor reflect indexed files only.",
            prep.indexed_files,
            scan_stats.crawled_files,
            prep.indexed_files * 100 / scan_stats.crawled_files,
            if scan_stats.unindexed_exts.is_empty() {
                "(none)".to_string()
            } else {
                scan_stats.unindexed_exts.join(", ")
            }
        );
    }
    Ok(g)
}

fn preview(paths: &[String]) -> String {
    const MAX: usize = 3;
    if paths.len() <= MAX {
        return paths.join(", ");
    }
    format!(
        "{}, and {} more",
        paths[..MAX].join(", "),
        paths.len() - MAX
    )
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
