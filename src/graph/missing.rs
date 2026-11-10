use anyhow::Result;
use ignore::WalkBuilder;
use std::path::Path;

/// Split a `missing` pattern into literal alternatives: `Remote|serverId`
/// matches files containing either substring. Literals, not regex — no new
/// syntax to learn, no new dependency, and the gate pattern works as-is.
pub fn split_alternatives(raw: &str) -> Vec<&str> {
    raw.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// True when `content` contains any alternative (or the defines marker).
fn contains_any(content: &str, alts: &[&str]) -> bool {
    alts.iter().any(|a| content.contains(a))
}

/// Pure core: relative paths of files whose content lacks every alternative.
/// `files` is `(rel_path, content)`; `defines` pre-filters to files
/// containing the marker (e.g. only lane-defining files).
pub fn missing_in_files(
    files: &[(String, String)],
    pattern: &str,
    defines: Option<&str>,
) -> Vec<String> {
    let alts = split_alternatives(pattern);
    let mut out: Vec<String> = files
        .iter()
        .filter(|(_, content)| match defines {
            Some(d) => content.contains(d),
            None => true,
        })
        .filter(|(_, content)| !contains_any(content, &alts))
        .map(|(rel, _)| rel.clone())
        .collect();
    out.sort();
    out
}

/// Sweep `scope` (workspace-relative dir, or the whole root) for supported
/// source files lacking `pattern`. A walker sweep, not graph work: the
/// decisive fact of an absence investigation, unaskable in up/down/blast.
pub fn find_missing(
    root: &Path,
    scope: Option<&str>,
    pattern: &str,
    defines: Option<&str>,
) -> Result<Vec<String>> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let clean_scope = scope
        .map(|s| s.trim_start_matches("./").trim_end_matches('/'))
        .filter(|s| !s.is_empty() && *s != ".");

    let scan_root = match clean_scope {
        Some(sc) => crate::workspace::walker::confine_to_workspace(&canon_root, Path::new(sc))?,
        None => canon_root.clone(),
    };

    if !scan_root.exists() {
        return Ok(Vec::new());
    }

    let alts = split_alternatives(pattern);
    let mut builder = WalkBuilder::new(&scan_root);
    builder
        .follow_links(false)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".mimoriignore");
    if scan_root != canon_root {
        let root_ignore = canon_root.join(".mimoriignore");
        if root_ignore.is_file() {
            let _ = builder.add_ignore(root_ignore);
        }
    }

    let mut out = Vec::new();
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(rel_path) = path.strip_prefix(&canon_root) else {
            continue;
        };
        if crate::workspace::walker::is_ignored_rel(rel_path) {
            continue;
        }
        if !crate::workspace::walker::has_supported_extension(path) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(d) = defines {
            if !content.contains(d) {
                continue;
            }
        }
        if !contains_any(&content, &alts) {
            out.push(rel_path.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<(String, String)> {
        vec![
            ("a.ts".into(), "lane Remote branch".into()),
            ("b.ts".into(), "lane serverId branch".into()),
            ("web-server.ts".into(), "lane local branch".into()),
        ]
    }

    #[test]
    fn missing_returns_only_files_lacking_every_alternative() {
        assert_eq!(
            missing_in_files(&files(), "Remote|serverId", None),
            vec!["web-server.ts".to_string()]
        );
    }

    #[test]
    fn defines_prefilters_to_marker_files() {
        let mut fs = files();
        fs.push(("other.ts".into(), "nothing here".into()));
        assert_eq!(
            missing_in_files(&fs, "Remote|serverId", Some("lane")),
            vec!["web-server.ts".to_string()]
        );
    }

    #[test]
    fn empty_pattern_matches_nothing_so_everything_is_missing() {
        assert_eq!(missing_in_files(&files(), "", None).len(), 3);
    }

    #[test]
    fn find_missing_dot_scope_sweeps_entire_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.ts"), "const x = 1;").unwrap();
        std::fs::write(root.join("b.ts"), "const y = 2; // target").unwrap();
        let missing = find_missing(root, Some("."), "target", None).unwrap();
        assert_eq!(missing, vec!["a.ts".to_string()]);
        let missing_slash = find_missing(root, Some("./"), "target", None).unwrap();
        assert_eq!(missing_slash, vec!["a.ts".to_string()]);
    }
}
