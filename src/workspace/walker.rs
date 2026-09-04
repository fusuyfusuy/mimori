use crate::model::Symbol;
use crate::parser::parse_file;
use anyhow::Result;
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

pub fn discover_and_parse_workspace(root: &Path) -> Result<(Vec<PathBuf>, Vec<Symbol>)> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".mimoriignore");

    let mut discovered_files = Vec::new();

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
                let rel_path = path.strip_prefix(root).unwrap_or(path).to_path_buf();
                discovered_files.push(rel_path);
            }
        }
    }

    // Parallel parse all files
    let symbols: Vec<Symbol> = discovered_files
        .par_iter()
        .flat_map(|rel_path| {
            let full_path = root.join(rel_path);
            let content = match fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };

            let file_str = rel_path.to_string_lossy();
            match parse_file(&full_path, &content) {
                Ok(mut syms) => {
                    for s in &mut syms {
                        s.file = file_str.to_string();
                    }
                    syms
                }
                Err(_) => Vec::new(),
            }
        })
        .collect();

    Ok((discovered_files, symbols))
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
