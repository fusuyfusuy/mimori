use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Maximum file size indexed or scanned (16 MiB) to prevent OOM on giant blobs.
pub const MAX_INDEX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Maximum symlink indirections followed while resolving a confined path.
const MAX_SYMLINK_HOPS: usize = 40;

/// Read at most `max_bytes` from `path` using a bounded reader (zero TOCTOU).
/// Returns `Ok(None)` when the file exceeds `max_bytes`.
pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// Read a UTF-8 file of at most `max_bytes` using a bounded reader (zero TOCTOU).
/// Returns `Ok(None)` when the file exceeds `max_bytes`.
pub fn read_to_string_bounded(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let Some(bytes) = read_bounded(path, max_bytes)? else {
        return Ok(None);
    };
    Ok(Some(String::from_utf8(bytes)?))
}

pub(crate) fn is_io_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// Directory names never indexed, matched as whole path components relative to
/// the workspace root.
const IGNORED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "dist",
    "build",
    "vendor",
    ".mimori",
];

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go",
];

/// Coverage accounting: what the walker crawled vs indexed.
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub indexed_files: usize,
    pub crawled_files: usize,
    pub unindexed_exts: Vec<String>,
}

/// Single-pass workspace discovery collecting supported source files and
/// workspace manifest/config paths without redundant directory walks.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceDiscovery {
    /// `(normalized_rel, full_path, mtime, size_bytes)` sorted deterministically by `normalized_rel`.
    pub supported: Vec<(String, PathBuf, i64, u64)>,
    pub package_manifests: Vec<PathBuf>,
    pub tsconfigs: Vec<PathBuf>,
    pub cargo_manifests: Vec<PathBuf>,
    pub go_mods: Vec<PathBuf>,
    pub stats: ScanStats,
}

/// Hashed file entry produced by `hash_discovered_files`.
pub struct HashedFile {
    pub rel: String,
    pub full: PathBuf,
    pub mtime: i64,
    pub hash: String,
}

/// Symlink handling policy for `confine_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Allow symlinks whose canonical target stays within the workspace root.
    AllowWithinWorkspace,
    /// Forbid symlinks anywhere along the path from workspace root to candidate.
    Forbid,
}

/// Validated, workspace-confined `--scope` prefix and optional canonical directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceScope {
    /// Normalized workspace-relative prefix (forward slashes, no leading/trailing `/`).
    /// Empty string means whole workspace (`.`).
    pub rel_prefix: String,
    /// Canonical filesystem path when the scope resolves to an existing file/directory.
    pub canon_path: Option<PathBuf>,
    /// Secondary workspace-relative prefix when `--scope` traversed an internal symlink.
    alias_rel_prefix: Option<String>,
}

impl WorkspaceScope {
    /// Parse and validate a user-supplied `--scope` string against `workspace_root`.
    /// Rejects empty strings, absolute paths, `..` traversal, and escaping symlinks.
    pub fn parse(workspace_root: &Path, raw: &str, require_exists: bool) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("Scope cannot be empty.");
        }
        let normalized_sep = trimmed.replace('\\', "/");
        if normalized_sep.starts_with('/')
            || Path::new(trimmed).is_absolute()
            || has_windows_drive_prefix(&normalized_sep)
        {
            bail!("Scope escapes workspace: {}", raw);
        }

        let mut parts: Vec<&str> = Vec::new();
        for seg in normalized_sep.split('/') {
            match seg {
                "" | "." => {}
                ".." => bail!("Scope escapes workspace: {}", raw),
                other => parts.push(other),
            }
        }

        let canon_root = workspace_root
            .canonicalize()
            .with_context(|| format!("Invalid workspace root '{}'", workspace_root.display()))?;

        if parts.is_empty() {
            return Ok(Self {
                rel_prefix: String::new(),
                canon_path: Some(canon_root),
                alias_rel_prefix: None,
            });
        }

        let raw_rel = parts.join("/");
        match confine_path_inner(
            workspace_root,
            &canon_root,
            Path::new(&raw_rel),
            SymlinkPolicy::AllowWithinWorkspace,
        ) {
            Ok(canon) => {
                let canon_rel = canon
                    .strip_prefix(&canon_root)
                    .map(normalize_rel_str)
                    .unwrap_or_else(|_| raw_rel.clone());
                let (rel_prefix, alias_rel_prefix) = if canon_rel == raw_rel {
                    (raw_rel, None)
                } else if canon.is_dir() {
                    (canon_rel, Some(raw_rel))
                } else {
                    (raw_rel, Some(canon_rel))
                };
                Ok(Self {
                    rel_prefix,
                    canon_path: Some(canon),
                    alias_rel_prefix,
                })
            }
            Err(ConfineError::NotFound(_)) if !require_exists => Ok(Self {
                rel_prefix: raw_rel,
                canon_path: None,
                alias_rel_prefix: None,
            }),
            Err(ConfineError::NotFound(_)) => {
                bail!("Scope path '{}' does not exist in workspace.", raw);
            }
            Err(other) => Err(other.into_anyhow()),
        }
    }

    /// True if `rel` (workspace-relative forward-slash path) is inside this scope
    /// on a strict path-component boundary.
    pub fn matches_rel(&self, rel: &str) -> bool {
        if self.rel_prefix.is_empty() {
            return true;
        }
        let norm = rel.strip_prefix("./").unwrap_or(rel);
        if matches_path_prefix(norm, &self.rel_prefix) {
            return true;
        }
        self.alias_rel_prefix
            .as_deref()
            .is_some_and(|alias| alias.is_empty() || matches_path_prefix(norm, alias))
    }
}

fn has_windows_drive_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn matches_path_prefix(norm: &str, prefix: &str) -> bool {
    norm == prefix
        || norm
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Hash all discovered source files in parallel, dropping file byte buffers
/// immediately so clean workspaces retain zero source contents in heap memory.
pub fn hash_discovered_files(
    discovery: &WorkspaceDiscovery,
) -> (Vec<HashedFile>, Vec<String>, usize) {
    enum ReadOutcome {
        Hashed(HashedFile),
        TransientErr(String),
        Gone,
    }

    let outcomes: Vec<ReadOutcome> = discovery
        .supported
        .par_iter()
        .map(|(rel, full, mtime, size)| {
            if *size > MAX_INDEX_FILE_BYTES {
                return ReadOutcome::TransientErr(rel.clone());
            }
            match read_bounded(full, MAX_INDEX_FILE_BYTES) {
                Ok(Some(bytes)) => {
                    let hash = format!("{:x}", fnv1a_hash(&bytes));
                    ReadOutcome::Hashed(HashedFile {
                        rel: rel.clone(),
                        full: full.clone(),
                        mtime: *mtime,
                        hash,
                    })
                }
                Ok(None) => ReadOutcome::TransientErr(rel.clone()),
                Err(e) if is_io_not_found(&e) => ReadOutcome::Gone,
                Err(_) => ReadOutcome::TransientErr(rel.clone()),
            }
        })
        .collect();

    let mut hashed = Vec::with_capacity(outcomes.len());
    let mut transient_failed = Vec::new();
    for outcome in outcomes {
        match outcome {
            ReadOutcome::Hashed(h) => hashed.push(h),
            ReadOutcome::TransientErr(rel) => transient_failed.push(rel),
            ReadOutcome::Gone => {}
        }
    }
    let indexed_count = hashed.len() + transient_failed.len();
    (hashed, transient_failed, indexed_count)
}

fn resolve_entry_meta(
    entry: &ignore::DirEntry,
    ft: fs::FileType,
    canon_root: Option<&Path>,
) -> Option<fs::Metadata> {
    if ft.is_symlink() {
        let cr = canon_root?;
        let canon_target = entry.path().canonicalize().ok()?;
        if !canon_target.starts_with(cr) {
            return None;
        }
        if let Ok(target_rel) = canon_target.strip_prefix(cr) {
            if is_ignored_rel(target_rel) {
                return None;
            }
        }
        let target_meta = fs::metadata(&canon_target).ok()?;
        return target_meta.is_file().then_some(target_meta);
    }
    if ft.is_file() {
        return entry.metadata().ok();
    }
    None
}

fn classify_manifest(
    path: &Path,
    package_manifests: &mut Vec<PathBuf>,
    tsconfigs: &mut Vec<PathBuf>,
    cargo_manifests: &mut Vec<PathBuf>,
    go_mods: &mut Vec<PathBuf>,
) {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if name == "package.json" {
        package_manifests.push(path.to_path_buf());
    } else if name == "jsconfig.json" || (name.starts_with("tsconfig") && name.ends_with(".json")) {
        tsconfigs.push(path.to_path_buf());
    } else if name == "Cargo.toml" {
        cargo_manifests.push(path.to_path_buf());
    } else if name == "go.mod" {
        go_mods.push(path.to_path_buf());
    }
}

pub fn discover_workspace(root: &Path) -> WorkspaceDiscovery {
    let builder = walk_builder(root);
    let canon_root = root.canonicalize().ok();

    let mut supported = Vec::new();
    let mut package_manifests = Vec::new();
    let mut tsconfigs = Vec::new();
    let mut cargo_manifests = Vec::new();
    let mut go_mods = Vec::new();
    let mut crawled = 0usize;
    let mut ext_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for entry in builder.build().flatten() {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        let Some(meta) = resolve_entry_meta(&entry, ft, canon_root.as_deref()) else {
            continue;
        };
        let path = entry.path();
        let rel_path = path.strip_prefix(root).unwrap_or(path);
        if is_ignored_rel(rel_path) {
            continue;
        }

        classify_manifest(
            path,
            &mut package_manifests,
            &mut tsconfigs,
            &mut cargo_manifests,
            &mut go_mods,
        );

        crawled += 1;
        if !has_supported_extension(path) {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)")
                .to_string();
            *ext_counts.entry(ext).or_insert(0) += 1;
            continue;
        }

        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let size = meta.len();

        let rel_norm = normalize_rel_str(rel_path);
        supported.push((rel_norm, path.to_path_buf(), mtime, size));
    }

    supported.sort_by(|a, b| a.0.cmp(&b.0));
    package_manifests.sort();
    tsconfigs.sort();
    cargo_manifests.sort();
    go_mods.sort();

    let mut unindexed_exts: Vec<String> = ext_counts
        .into_iter()
        .map(|(ext, n)| format!("{ext}×{n}"))
        .collect();
    unindexed_exts.sort();

    let indexed_files = supported.len();
    WorkspaceDiscovery {
        supported,
        package_manifests,
        tsconfigs,
        cargo_manifests,
        go_mods,
        stats: ScanStats {
            indexed_files,
            crawled_files: crawled,
            unindexed_exts,
        },
    }
}

pub fn normalize_rel_str(rel: &Path) -> String {
    let raw = rel.to_string_lossy();
    if raw.contains('\\') {
        raw.replace('\\', "/")
    } else {
        raw.into_owned()
    }
}

pub fn has_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext))
}

/// Shared walk configuration honoring `.gitignore` (even outside `.git`) and `.mimoriignore`.
pub fn walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".mimoriignore");
    builder
}

/// True when any component of the workspace-relative path is an ignored directory name.
pub fn is_ignored_rel(rel: &Path) -> bool {
    rel.components().any(|c| match c {
        Component::Normal(name) => IGNORED_DIRS.iter().any(|d| OsStr::new(d) == name),
        _ => false,
    })
}

pub fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Find the workspace root starting from `target_dir` (if provided) or `cwd`.
/// Prioritizes VCS root (`.git`, `.hg`) then `.mimori`. Never adopts an
/// out-of-workspace root on CLI commands (callers pass `None` to anchor to `cwd`).
pub fn find_workspace_root(target_dir: Option<&Path>, cwd: &Path) -> PathBuf {
    let start_dir = target_dir.unwrap_or(cwd);

    for marker in [".git", ".hg", ".mimori"] {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            if is_disallowed_workspace_root(d) {
                break;
            }
            if d.join(marker).exists() {
                return d.to_path_buf();
            }
            dir = d.parent();
        }
    }

    if let Some(target) = target_dir {
        if target.starts_with(cwd) && !is_disallowed_workspace_root(cwd) {
            return cwd.to_path_buf();
        }
        target.to_path_buf()
    } else {
        cwd.to_path_buf()
    }
}

fn same_dir_or_canon(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(ca), Ok(cb)) if ca == cb
        )
}

pub fn is_home_directory(path: &Path) -> bool {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && same_dir_or_canon(path, Path::new(&home)) {
            return true;
        }
    }
    #[cfg(windows)]
    if let Ok(up) = std::env::var("USERPROFILE") {
        if !up.is_empty() && same_dir_or_canon(path, Path::new(&up)) {
            return true;
        }
    }
    false
}

pub fn is_system_directory(path: &Path) -> bool {
    const EXACT_CONTAINERS: &[&str] = &[
        "/",
        "/home",
        "/Users",
        "/var",
        "/tmp",
        "/var/tmp",
        "/private",
        "/private/tmp",
        "/private/var",
        "/private/var/tmp",
    ];
    const SYSTEM_PREFIXES: &[&str] = &[
        "/etc",
        "/private/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/sys",
        "/proc",
        "/dev",
        "/boot",
        "/root",
        "/run",
        "/var/log",
        "/var/lib",
        "/var/run",
        "/var/cache",
        "/var/spool",
        "/var/lock",
        "/var/backups",
        "/var/db",
        "/System",
        "/Library",
        "/Volumes",
    ];
    let matches_sys = |p: &Path| {
        EXACT_CONTAINERS.iter().any(|&c| p == Path::new(c))
            || SYSTEM_PREFIXES.iter().any(|&pref| p.starts_with(pref))
    };
    if matches_sys(path) {
        return true;
    }
    if let Ok(canon) = path.canonicalize() {
        if matches_sys(&canon) {
            return true;
        }
    }
    false
}

pub fn is_disallowed_workspace_root(path: &Path) -> bool {
    is_system_directory(path)
        || is_home_directory(path)
        || same_dir_or_canon(path, &std::env::temp_dir())
}

/// Lexically collapse `.` and `..` without touching disk, so escaping relative
/// paths are caught BEFORE `canonicalize()` (eliminating existence oracles).
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out
}

enum ConfineError {
    Escape(String),
    NotFound(String),
    Other(String),
}

impl ConfineError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            ConfineError::Escape(m) | ConfineError::NotFound(m) | ConfineError::Other(m) => {
                anyhow::anyhow!(m)
            }
        }
    }
}

fn strip_workspace_prefix(path: &Path, canon_root: &Path, raw_root: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(canon_root) {
        return Some(rel.to_path_buf());
    }
    if raw_root.is_absolute() {
        if let Ok(rel) = path.strip_prefix(raw_root) {
            return Some(rel.to_path_buf());
        }
        let norm_raw = normalize_lexical(raw_root);
        if norm_raw.is_absolute() {
            if let Ok(rel) = path.strip_prefix(&norm_raw) {
                return Some(rel.to_path_buf());
            }
        }
    } else if let Ok(cwd) = std::env::current_dir() {
        let abs_raw = normalize_lexical(&cwd.join(raw_root));
        if let Ok(rel) = path.strip_prefix(&abs_raw) {
            return Some(rel.to_path_buf());
        }
    }
    None
}

fn check_remaining_for_escape(
    canon_root: &Path,
    current: &Path,
    remaining: &[Component<'_>],
    original: &Path,
) -> std::result::Result<(), ConfineError> {
    let mut sim = current.to_path_buf();
    for comp in remaining {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if sim == canon_root || !sim.pop() || !sim.starts_with(canon_root) {
                    return Err(ConfineError::Escape(format!(
                        "Path escapes workspace: {}",
                        original.display()
                    )));
                }
            }
            Component::Normal(seg) => sim.push(seg),
            Component::RootDir | Component::Prefix(_) => {
                return Err(ConfineError::Escape(format!(
                    "Path escapes workspace: {}",
                    original.display()
                )));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_symlink_step(
    raw_root: &Path,
    canon_root: &Path,
    original: &Path,
    policy: SymlinkPolicy,
    current: &mut PathBuf,
    hops: &mut usize,
) -> std::result::Result<(), ConfineError> {
    if policy == SymlinkPolicy::Forbid {
        return Err(ConfineError::Escape(format!(
            "Path escapes workspace (symlink forbidden): {}",
            original.display()
        )));
    }
    *hops += 1;
    if *hops > MAX_SYMLINK_HOPS {
        return Err(ConfineError::Other(format!(
            "Too many symlink levels resolving '{}'",
            original.display()
        )));
    }
    let link_target = fs::read_link(current.as_path()).map_err(|e| {
        ConfineError::Other(format!(
            "Cannot resolve path '{}': {}",
            original.display(),
            e
        ))
    })?;
    current.pop();

    let rel_target = if link_target.is_absolute() {
        let Some(rel) = strip_workspace_prefix(&link_target, canon_root, raw_root) else {
            return Err(ConfineError::Escape(format!(
                "Path escapes workspace: {}",
                original.display()
            )));
        };
        *current = canon_root.to_path_buf();
        rel
    } else {
        link_target
    };

    let target_comps: Vec<Component<'_>> = rel_target.components().collect();
    resolve_confined_steps(
        raw_root,
        canon_root,
        original,
        policy,
        current,
        &target_comps,
        hops,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_confined_steps(
    raw_root: &Path,
    canon_root: &Path,
    original: &Path,
    policy: SymlinkPolicy,
    current: &mut PathBuf,
    components: &[Component<'_>],
    hops: &mut usize,
) -> std::result::Result<(), ConfineError> {
    for (idx, comp) in components.iter().enumerate() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if *current == canon_root || !current.pop() || !current.starts_with(canon_root) {
                    return Err(ConfineError::Escape(format!(
                        "Path escapes workspace: {}",
                        original.display()
                    )));
                }
            }
            Component::Normal(seg) => {
                current.push(seg);
                let rest = &components[idx + 1..];
                match fs::symlink_metadata(&*current) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        resolve_symlink_step(
                            raw_root, canon_root, original, policy, current, hops,
                        )?;
                        if !rest.is_empty()
                            && !fs::symlink_metadata(&*current).is_ok_and(|m| m.is_dir())
                        {
                            check_remaining_for_escape(canon_root, current, rest, original)?;
                            return Err(ConfineError::Other(format!(
                                "Cannot resolve path '{}': Not a directory",
                                original.display()
                            )));
                        }
                    }
                    Ok(meta) => {
                        if !rest.is_empty() && !meta.is_dir() {
                            check_remaining_for_escape(canon_root, current, rest, original)?;
                            return Err(ConfineError::Other(format!(
                                "Cannot resolve path '{}': Not a directory",
                                original.display()
                            )));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        check_remaining_for_escape(canon_root, current, rest, original)?;
                        return Err(ConfineError::NotFound(format!(
                            "Cannot resolve path '{}': {}",
                            original.display(),
                            e
                        )));
                    }
                    Err(e) => {
                        check_remaining_for_escape(canon_root, current, rest, original)?;
                        return Err(ConfineError::Other(format!(
                            "Cannot resolve path '{}': {}",
                            original.display(),
                            e
                        )));
                    }
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ConfineError::Escape(format!(
                    "Path escapes workspace: {}",
                    original.display()
                )));
            }
        }
    }
    Ok(())
}

fn confine_path_inner(
    raw_root: &Path,
    canon_root: &Path,
    candidate: &Path,
    policy: SymlinkPolicy,
) -> std::result::Result<PathBuf, ConfineError> {
    let rel_candidate = if candidate.is_absolute() {
        let Some(rel) = strip_workspace_prefix(candidate, canon_root, raw_root) else {
            return Err(ConfineError::Escape(format!(
                "Path escapes workspace: {}",
                candidate.display()
            )));
        };
        rel
    } else {
        candidate.to_path_buf()
    };

    let mut current = canon_root.to_path_buf();
    let mut hops = 0usize;
    let comps: Vec<Component<'_>> = rel_candidate.components().collect();
    resolve_confined_steps(
        raw_root,
        canon_root,
        candidate,
        policy,
        &mut current,
        &comps,
        &mut hops,
    )?;

    if current.starts_with(canon_root) {
        Ok(current)
    } else {
        Err(ConfineError::Escape(format!(
            "Path escapes workspace: {}",
            candidate.display()
        )))
    }
}

/// Confine `candidate` strictly within `workspace_root` under `policy`.
/// Resolves path components step-by-step from `canon_root` so escaping `..`
/// or symlinks fail immediately before touching disk outside the workspace
/// (zero external existence oracle, strict `SymlinkPolicy::Forbid`).
pub fn confine_path(
    workspace_root: &Path,
    candidate: &Path,
    policy: SymlinkPolicy,
) -> Result<PathBuf> {
    let canon_root = workspace_root
        .canonicalize()
        .with_context(|| format!("Invalid workspace root '{}'", workspace_root.display()))?;
    confine_path_inner(workspace_root, &canon_root, candidate, policy)
        .map_err(ConfineError::into_anyhow)
}

pub fn confine_to_workspace(workspace_root: &Path, candidate: &Path) -> Result<PathBuf> {
    confine_path(
        workspace_root,
        candidate,
        SymlinkPolicy::AllowWithinWorkspace,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_match_components_not_substrings() {
        assert!(is_ignored_rel(Path::new("target/debug/x.rs")));
        assert!(is_ignored_rel(Path::new("a/node_modules/b/c.js")));
        assert!(is_ignored_rel(Path::new(".mimori/index.rs")));
        assert!(!is_ignored_rel(Path::new("src/lib.rs")));
        assert!(!is_ignored_rel(Path::new("src/building/mod.rs")));
        assert!(!is_ignored_rel(Path::new("src/targeting.rs")));
    }

    #[test]
    fn extension_filter_covers_the_supported_set() {
        for ext in SUPPORTED_EXTENSIONS {
            assert!(has_supported_extension(Path::new(&format!("a.{ext}"))));
        }
        assert!(!has_supported_extension(Path::new("a.md")));
        assert!(!has_supported_extension(Path::new("noext")));
    }

    #[test]
    fn workspace_root_walks_up_to_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let nested = root.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(".mimori")).unwrap();
        assert_eq!(
            find_workspace_root(Some(&nested), Path::new("/nowhere")),
            root
        );
    }

    #[test]
    fn workspace_root_prefers_a_containing_cwd_over_the_file_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().canonicalize().unwrap();
        let nested = cwd.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_workspace_root(Some(&nested), &cwd), cwd);
    }

    #[test]
    fn discovery_stats_report_indexed_vs_crawled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("b.md"), "# docs").unwrap();
        fs::write(root.join("c.json"), "{}").unwrap();
        let disc = discover_workspace(root);
        assert_eq!(disc.supported.len(), 1);
        assert_eq!(disc.stats.indexed_files, 1);
        assert_eq!(disc.stats.crawled_files, 3);
        assert!(disc
            .stats
            .unindexed_exts
            .iter()
            .any(|e| e.starts_with("md")));
        assert!(disc
            .stats
            .unindexed_exts
            .iter()
            .any(|e| e.starts_with("json")));
    }

    #[test]
    fn hash_distinguishes_same_length_content() {
        assert_ne!(
            fnv1a_hash(b"pub fn aaa() {}"),
            fnv1a_hash(b"pub fn bbb() {}")
        );
    }

    #[test]
    fn home_and_system_directories_are_disallowed_workspace_roots() {
        assert!(is_disallowed_workspace_root(Path::new("/")));
        assert!(is_disallowed_workspace_root(Path::new("/home")));
        assert!(is_disallowed_workspace_root(Path::new("/etc")));
        assert!(is_disallowed_workspace_root(Path::new("/usr")));
        assert!(is_disallowed_workspace_root(Path::new("/run")));
        assert!(is_disallowed_workspace_root(Path::new("/var/run")));

        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                assert!(is_home_directory(Path::new(&home)));
                assert!(is_disallowed_workspace_root(Path::new(&home)));
            }
        }
    }

    #[test]
    fn confine_path_eliminates_relative_existence_oracle() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let err = confine_to_workspace(&ws, Path::new("../nonexistent_secret_999.txt"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes workspace"), "got: {err}");
    }

    #[test]
    fn workspace_scope_rejects_escapes_and_matches_prefixes_strictly() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        assert!(WorkspaceScope::parse(ws, "../..", false).is_err());
        assert!(WorkspaceScope::parse(ws, "/", false).is_err());
        assert!(WorkspaceScope::parse(ws, "", false).is_err());

        let sc = WorkspaceScope::parse(ws, "src/graph", false).unwrap();
        assert!(sc.matches_rel("src/graph/mod.rs"));
        assert!(!sc.matches_rel("src/graph_extra/mod.rs"));
        assert!(!sc.matches_rel("other/src/graph/mod.rs"));

        let dot = WorkspaceScope::parse(ws, ".", false).unwrap();
        assert!(dot.matches_rel("anything/at/all.rs"));
    }

    #[test]
    fn marker_search_does_not_escape_into_home_directory() {
        if let Ok(home) = std::env::var("HOME") {
            let home_path = PathBuf::from(&home);
            let child = home_path.join("non_existent_subdir_for_mimori_test_12345");
            let root = find_workspace_root(None, &child);
            assert_ne!(root, home_path);
        }
    }
}
