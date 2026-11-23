use crate::workspace::walker::{is_disallowed_workspace_root, WorkspaceScope};
use anyhow::{bail, Result};
use std::path::Path;

const MAX_MISSING_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MISSING_RESULTS: usize = 1000;

/// Split a `missing` pattern into literal alternatives: `Remote|serverId`
/// matches files containing either substring.
pub fn split_alternatives(raw: &str) -> Vec<&str> {
    raw.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn contains_any(content: &str, alts: &[&str]) -> bool {
    alts.iter().any(|a| content.contains(a))
}

/// Format missing-pattern file results as concise Markdown shared by CLI and MCP.
pub fn format_missing_markdown(
    pattern: &str,
    scope: Option<&str>,
    defines: Option<&str>,
    missing: &[String],
) -> String {
    let where_str = scope.unwrap_or(".");
    let mut out = match defines {
        Some(d) if !d.is_empty() => format!(
            "### Missing `{}` under `{}` (defines: `{}`, {} files lack it)\n\n",
            pattern,
            where_str,
            d,
            missing.len()
        ),
        _ => format!(
            "### Missing `{}` under `{}` ({} files lack it)\n\n",
            pattern,
            where_str,
            missing.len()
        ),
    };
    if missing.is_empty() {
        out.push_str("No files missing the pattern.\n");
    } else {
        for f in missing {
            out.push_str(&format!("- `{}`\n", f));
        }
        if missing.len() >= MAX_MISSING_RESULTS {
            out.push_str(&format!(
                "\n_Capped at {} files; narrow --scope to see all._\n",
                MAX_MISSING_RESULTS
            ));
        }
    }
    out
}

/// Pure core: relative paths of files whose content lacks every alternative.
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
/// source files lacking `pattern`.
pub fn find_missing(
    root: &Path,
    scope: Option<&str>,
    pattern: &str,
    defines: Option<&str>,
) -> Result<Vec<String>> {
    if is_disallowed_workspace_root(root) {
        bail!(
            "Refusing to sweep home or system directory '{}' as workspace",
            root.display()
        );
    }
    let alts = split_alternatives(pattern);
    if alts.is_empty() {
        bail!("Pattern must contain at least one non-empty substring.");
    }
    if let Some(d) = defines {
        if d.trim().is_empty() {
            bail!("--defines must not be empty.");
        }
    }

    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let scan_root = match scope {
        Some(sc) => {
            let parsed = WorkspaceScope::parse(&canon_root, sc, true)?;
            parsed.canon_path.unwrap_or_else(|| canon_root.clone())
        }
        None => canon_root.clone(),
    };

    let mut builder = crate::workspace::walker::walk_builder(&scan_root);
    builder.follow_links(false);
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
        if entry
            .metadata()
            .is_ok_and(|m| m.len() > MAX_MISSING_FILE_BYTES)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        if let Some(d) = defines {
            if !content.contains(d) {
                continue;
            }
        }
        if !contains_any(&content, &alts) {
            out.push(crate::workspace::walker::normalize_rel_str(rel_path));
        }
    }
    out.sort();
    out.truncate(MAX_MISSING_RESULTS);
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
    fn empty_pattern_matches_nothing_in_pure_helper() {
        assert_eq!(missing_in_files(&files(), "", None).len(), 3);
    }

    #[test]
    fn find_missing_validates_scope_and_handles_non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.ts"), "const x = 1;").unwrap();
        std::fs::write(root.join("b.ts"), "const y = 2; // target").unwrap();
        std::fs::write(root.join("latin1.ts"), b"// caf\xe9\nconst z = 3;").unwrap();

        assert!(find_missing(root, Some("/"), "target", None).is_err());
        assert!(find_missing(root, Some("../"), "target", None).is_err());
        assert!(find_missing(root, None, "target", Some("")).is_err());

        let missing = find_missing(root, Some("."), "target", None).unwrap();
        assert_eq!(missing, vec!["a.ts".to_string(), "latin1.ts".to_string()]);
    }
}
