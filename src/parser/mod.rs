pub mod go;
pub mod lang;
pub mod python;
pub mod rust;
pub mod typescript;

pub use lang::Language;

use crate::model::Symbol;
use crate::workspace::AliasSet;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::Path;

pub fn parse_file(path: &Path, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let lang = Language::from_path(path);
    let file_str = path.to_str().unwrap_or("");

    match lang {
        Language::Rust => rust::parse_rust(file_str, content, aliases),
        Language::TypeScript | Language::JavaScript => {
            typescript::parse_typescript(file_str, content, false, aliases)
        }
        Language::TSX => typescript::parse_typescript(file_str, content, true, aliases),
        Language::Python => python::parse_python(file_str, content),
        Language::Go => go::parse_go(file_str, content),
        Language::Unknown => bail!("Unsupported language for file: {}", path.display()),
    }
}

pub(crate) fn spec_to_module_hint(spec: &str) -> String {
    let mut s = spec.trim();
    while let Some(rest) = s.strip_prefix("./").or_else(|| s.strip_prefix("../")) {
        s = rest;
    }
    if s.starts_with(['@', '~', '$']) {
        if let Some((first, rest)) = s.split_once('/') {
            if first.len() <= 2 {
                s = rest;
            } else if first.starts_with('@') {
                if let Some((_, after_pkg)) = rest.split_once('/') {
                    s = after_pkg;
                }
            }
        }
    }
    s.trim_matches('/').to_string()
}

pub(crate) fn apply_import_aliases(
    symbols: &mut [Symbol],
    aliases: &HashMap<String, (String, String)>,
) {
    if aliases.is_empty() {
        return;
    }
    for s in symbols {
        for call in &mut s.calls {
            if s.member_calls.contains(call) {
                continue;
            }
            if let Some((hint, exported)) = aliases.get(call.as_str()) {
                let rewritten = format!("{hint}:{exported}");
                if let Some(cnt) = s.call_counts.remove(call.as_str()) {
                    *s.call_counts.entry(rewritten.clone()).or_insert(0) += cnt;
                }
                *call = rewritten;
            }
        }
        s.calls.dedup();
        for m in &mut s.mentions {
            if let Some((_, exported)) = aliases.get(m.as_str()) {
                *m = exported.clone();
            }
        }
        s.mentions.dedup();
    }
}
