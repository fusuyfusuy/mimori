use crate::graph::SymbolGraph;
use crate::model::Symbol;
use crate::parser::parse_file;
use crate::storage::db::{is_corruption_error, is_readonly_or_io_error, Database};
use crate::workspace::walker::{
    discover_workspace, fnv1a_hash, hash_discovered_files, is_disallowed_workspace_root,
    is_io_not_found, read_bounded, HashedFile, WorkspaceDiscovery, MAX_INDEX_FILE_BYTES,
};
use crate::workspace::AliasSet;
use anyhow::{bail, Result};
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Single-pass workspace discovery + FNV-1a content hashes.
pub struct PreparedWorkspace {
    pub discovery: WorkspaceDiscovery,
    pub aliases: AliasSet,
    pub hashed: Vec<HashedFile>,
    pub transient_failed: Vec<String>,
    pub indexed_files: usize,
    pub content_fingerprint: String,
}

/// Fast metadata-only fingerprint (`ino + size + mtime + ctime`) across all
/// supported files and workspace manifests (including `tsconfig` `extends`/
/// `references` targets, glob-discovered manifests, and `baseUrl` dirs).
/// On POSIX, kernel-maintained `ctime` changes on every write/rename/utime,
/// letting warm MCP requests skip source file reads when nothing moved.
/// On non-Unix platforms (lacking `ino`/`ctime`), emits a fresh nonce to
/// disable the fast path honestly and fall back to strict FNV-1a content hashes.
pub fn quick_stat_fingerprint(root: &Path) -> Result<String> {
    if is_disallowed_workspace_root(root) {
        bail!(
            "Refusing to index home or system directory '{}' as workspace root",
            root.display()
        );
    }

    #[cfg(not(unix))]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NONCE: AtomicU64 = AtomicU64::new(1);
        let n = NONCE.fetch_add(1, Ordering::Relaxed);
        return Ok(format!("nonunix-{n:016x}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let discovery = discover_workspace(root);
        let aliases = AliasSet::collect_with_discovery(root, &discovery);
        let mut fp: u64 = 0xcbf29ce484222325;
        let mix = |h: u64, bytes: &[u8]| fnv1a_hash(bytes) ^ h.wrapping_mul(0x100000001b3);
        let mix_meta = |mut h: u64, path: &Path| -> u64 {
            if let Ok(meta) = fs::metadata(path) {
                h = mix(h, &meta.len().to_le_bytes());
                if let Ok(m) = meta.modified() {
                    if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                        h = mix(h, &(d.as_nanos() as u64).to_le_bytes());
                    }
                }
                h = mix(h, &meta.ino().to_le_bytes());
                h = mix(h, &meta.ctime().to_le_bytes());
                h = mix(h, &meta.ctime_nsec().to_le_bytes());
            }
            h
        };

        fp = mix(fp, aliases.fingerprint().as_bytes());
        for (rel, full, _, _) in &discovery.supported {
            fp = mix(fp, rel.as_bytes());
            fp = mix_meta(fp, full);
        }
        for extra in aliases.visited_configs() {
            fp = mix(fp, extra.to_string_lossy().as_bytes());
            fp = mix_meta(fp, extra);
        }
        Ok(format!("{fp:016x}"))
    }
}

pub fn prepare_workspace(root: &Path) -> Result<PreparedWorkspace> {
    if is_disallowed_workspace_root(root) {
        bail!(
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

enum ParseOutcome {
    Parsed {
        rel: String,
        mtime: i64,
        hash: String,
        symbols: Option<Vec<Symbol>>,
    },
    Gone(String),
    TransientErr,
}

fn parse_files_parallel(stale: &[&HashedFile], aliases: &AliasSet) -> Vec<ParseOutcome> {
    stale
        .par_iter()
        .map(|h| match read_bounded(&h.full, MAX_INDEX_FILE_BYTES) {
            Ok(Some(bytes)) => {
                // Re-hash the exact bytes being parsed to eliminate read/hash TOCTOU.
                let actual_hash = format!("{:x}", fnv1a_hash(&bytes));
                let content = String::from_utf8_lossy(&bytes);
                let symbols = parse_file(&h.full, &content, aliases).ok().map(|mut syms| {
                    for s in &mut syms {
                        s.file = h.rel.clone();
                    }
                    syms
                });
                ParseOutcome::Parsed {
                    rel: h.rel.clone(),
                    mtime: h.mtime,
                    hash: actual_hash,
                    symbols,
                }
            }
            Ok(None) => ParseOutcome::TransientErr,
            Err(e) if is_io_not_found(&e) => ParseOutcome::Gone(h.rel.clone()),
            Err(_) => ParseOutcome::TransientErr,
        })
        .collect()
}

fn parse_in_memory(root: &Path, prep: &PreparedWorkspace) -> SymbolGraph {
    let all_refs: Vec<&HashedFile> = prep.hashed.iter().collect();
    let outcomes = parse_files_parallel(&all_refs, &prep.aliases);
    let mut symbols = Vec::new();
    for o in outcomes {
        if let ParseOutcome::Parsed {
            symbols: Some(syms),
            ..
        } = o
        {
            symbols.extend(syms);
        }
    }
    symbols.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.end_line.cmp(&b.end_line))
            .then_with(|| a.name.cmp(&b.name))
    });
    build_graph_with_meta(root, prep, symbols)
}

fn build_graph_with_meta(
    root: &Path,
    prep: &PreparedWorkspace,
    symbols: Vec<Symbol>,
) -> SymbolGraph {
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
    g
}

fn ensure_no_cache_symlinks(mimori_dir: &Path) -> Result<()> {
    for name in [
        "index.db",
        "index.db-wal",
        "index.db-shm",
        "index.db-journal",
        "index.db.lock",
    ] {
        let p = mimori_dir.join(name);
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if meta.file_type().is_symlink() {
                bail!("Refusing to use symlinked cache file '{}'", p.display());
            }
        }
    }
    Ok(())
}

pub fn sync_prepared_workspace(root: &Path, prep: &PreparedWorkspace) -> Result<SymbolGraph> {
    let mimori_dir = root.join(".mimori");
    if let Ok(meta) = fs::symlink_metadata(&mimori_dir) {
        if meta.file_type().is_symlink() {
            bail!(
                "Refusing to use symlinked cache directory '{}'",
                mimori_dir.display()
            );
        }
    } else if let Err(e) = fs::create_dir_all(&mimori_dir) {
        if e.kind() == std::io::ErrorKind::PermissionDenied
            || e.kind() == std::io::ErrorKind::ReadOnlyFilesystem
        {
            return Ok(parse_in_memory(root, prep));
        }
        return Err(e.into());
    }

    ensure_no_cache_symlinks(&mimori_dir)?;
    let db_path = mimori_dir.join("index.db");

    let db = match Database::open_or_rebuild(&db_path) {
        Ok(db) => db,
        Err(e) if is_readonly_or_io_error(&e) => {
            return Ok(parse_in_memory(root, prep));
        }
        Err(e) => return Err(e),
    };

    match run_db_sync(db, root, prep) {
        Ok(g) => Ok(g),
        Err(e) if is_corruption_error(&e) => {
            let fresh_db = match Database::rebuild_if_corrupt(&db_path) {
                Ok(db) => db,
                Err(re) if is_readonly_or_io_error(&re) => {
                    return Ok(parse_in_memory(root, prep));
                }
                Err(re) => return Err(re),
            };
            match run_db_sync(fresh_db, root, prep) {
                Ok(g) => Ok(g),
                Err(re) if is_readonly_or_io_error(&re) => Ok(parse_in_memory(root, prep)),
                Err(re) => Err(re),
            }
        }
        Err(e) if is_readonly_or_io_error(&e) => Ok(parse_in_memory(root, prep)),
        Err(e) => Err(e),
    }
}

fn run_db_sync(mut db: Database, root: &Path, prep: &PreparedWorkspace) -> Result<SymbolGraph> {
    let (stored_fp, snapshot_files) = db.read_sync_snapshot()?;
    let alias_changed = stored_fp.as_deref() != Some(prep.aliases.fingerprint());
    let db_files = if alias_changed {
        std::collections::HashMap::new()
    } else {
        snapshot_files
    };

    let mut disk_paths: HashSet<&str> =
        HashSet::with_capacity(prep.hashed.len() + prep.transient_failed.len());
    for failed_rel in &prep.transient_failed {
        disk_paths.insert(failed_rel.as_str());
    }

    let mut stale: Vec<&HashedFile> = Vec::new();
    for h in &prep.hashed {
        disk_paths.insert(h.rel.as_str());
        if alias_changed {
            stale.push(h);
        } else {
            let cached = db_files.get(&h.rel);
            if !matches!(cached, Some(db_hash) if *db_hash == h.hash) {
                stale.push(h);
            }
        }
    }

    let _p = crate::Phase::start("parse (parallel)");
    let outcomes = parse_files_parallel(&stale, &prep.aliases);
    drop(_p);

    let mut parsed_rows: Vec<(String, i64, String, Option<Vec<Symbol>>)> =
        Vec::with_capacity(outcomes.len());
    let mut gone_paths: Vec<String> = Vec::new();
    for o in outcomes {
        match o {
            ParseOutcome::Parsed {
                rel,
                mtime,
                hash,
                symbols,
            } => parsed_rows.push((rel, mtime, hash, symbols)),
            ParseOutcome::Gone(rel) => gone_paths.push(rel),
            ParseOutcome::TransientErr => {}
        }
    }

    let _p = crate::Phase::start("db write+load");
    let mut unparsed = Vec::new();
    let mut batch_updates = Vec::with_capacity(parsed_rows.len());
    for (rel, mtime, hash, symbols) in &parsed_rows {
        let syms: &[Symbol] = match symbols {
            Some(syms) => syms.as_slice(),
            None => {
                unparsed.push(rel.clone());
                &[]
            }
        };
        batch_updates.push((rel.as_str(), *mtime, hash.as_str(), syms));
    }

    let mut deleted_paths: Vec<&str> = Vec::new();
    if !alias_changed {
        for db_path_str in db_files.keys() {
            if !disk_paths.contains(db_path_str.as_str()) {
                deleted_paths.push(db_path_str.as_str());
            }
        }
        for gone in &gone_paths {
            deleted_paths.push(gone.as_str());
        }
    }

    let symbols = db.commit_sync_and_load(
        alias_changed,
        alias_changed.then_some(prep.aliases.fingerprint()),
        &batch_updates,
        &deleted_paths,
    )?;
    drop(_p);

    if !unparsed.is_empty() {
        eprintln!(
            "mimori: {} file(s) could not be parsed and are indexed as empty: {}",
            unparsed.len(),
            preview(&unparsed)
        );
    }

    Ok(build_graph_with_meta(root, prep, symbols))
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

fn remove_cache_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!(
                "Refusing to clean symlinked cache file '{}'",
                path.display()
            );
        }
        Ok(m) if m.is_file() => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
        _ => Ok(()),
    }
}

fn remove_cache_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!(
                "Refusing to clean symlinked cache directory '{}'",
                path.display()
            );
        }
        Ok(m) if m.is_dir() => match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
        _ => Ok(()),
    }
}

pub fn clean_cache(root: &Path, all: bool) -> Result<()> {
    if is_disallowed_workspace_root(root) {
        bail!(
            "Refusing to clean home or system directory '{}' as workspace root",
            root.display()
        );
    }

    let mimori_dir = root.join(".mimori");
    let meta = match fs::symlink_metadata(&mimori_dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        bail!(
            "Refusing to clean symlinked .mimori directory '{}'",
            mimori_dir.display()
        );
    }
    if !meta.is_dir() {
        return Ok(());
    }

    for f in [
        "index.db",
        "index.db-wal",
        "index.db-shm",
        "index.db-journal",
        "index.db.lock",
        "cache.bin",
    ] {
        remove_cache_file(&mimori_dir.join(f))?;
    }

    if all {
        remove_cache_dir(&mimori_dir.join(".cache"))?;
    }

    Ok(())
}
