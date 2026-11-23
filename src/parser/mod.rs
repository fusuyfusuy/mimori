pub mod common;
pub mod go;
pub mod lang;
pub mod python;
pub mod rust;
pub mod typescript;

pub use lang::Language;

use crate::model::Symbol;
use crate::workspace::AliasSet;
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn parse_file(path: &Path, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let lang = Language::from_path(path);
    let Some(file_str) = path.to_str() else {
        bail!("Non-UTF8 file path cannot be indexed: {}", path.display());
    };

    match lang {
        Language::Rust => rust::parse_rust(file_str, content, aliases),
        Language::TypeScript | Language::JavaScript => {
            typescript::parse_typescript(file_str, content, false, aliases)
        }
        Language::TSX => typescript::parse_typescript(file_str, content, true, aliases),
        Language::Python => python::parse_python(file_str, content, aliases),
        Language::Go => go::parse_go(file_str, content, aliases),
        Language::Unknown => bail!("Unsupported language for file: {}", path.display()),
    }
}

const SCRIPT_EXTS: &[&str] = &[".tsx", ".mts", ".cts", ".jsx", ".mjs", ".cjs", ".ts", ".js"];

fn strip_alias_prefix(s: &str) -> &str {
    if !s.starts_with(['@', '~', '$']) {
        return s;
    }
    let Some((first, rest)) = s.split_once('/') else {
        return s;
    };
    if first.starts_with(['~', '$']) || first.len() <= 2 {
        return rest;
    }
    if first.starts_with('@') {
        return match rest.split_once('/') {
            Some((_, after_pkg)) => after_pkg,
            None => rest,
        };
    }
    s
}

pub(crate) fn spec_to_module_hint(spec: &str) -> String {
    let mut s = spec.trim();
    while let Some(rest) = s.strip_prefix("./").or_else(|| s.strip_prefix("../")) {
        s = rest;
    }
    s = strip_alias_prefix(s).trim_matches('/');
    for ext in SCRIPT_EXTS {
        if let Some(stem) = s
            .strip_suffix(ext)
            .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
        {
            return stem.to_string();
        }
    }
    s.to_string()
}

pub(crate) fn format_hinted_call(hint: &str, name: &str) -> String {
    let hint = hint.trim();
    if hint.is_empty() {
        name.to_string()
    } else {
        format!("{hint}:{name}")
    }
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
            let rewritten = if let Some((hint, exported)) = aliases.get(call.as_str()) {
                Some(format_hinted_call(hint, exported))
            } else if let Some((head, tail)) = call.split_once("::") {
                aliases.get(head).map(|(hint, exported)| {
                    format_hinted_call(hint, &format!("{exported}::{tail}"))
                })
            } else {
                None
            };
            if let Some(new_call) = rewritten {
                if let Some(cnt) = s.call_counts.remove(call.as_str()) {
                    *s.call_counts.entry(new_call.clone()).or_insert(0) += cnt;
                }
                *call = new_call;
            }
        }
        let mut seen_calls = HashSet::with_capacity(s.calls.len());
        s.calls.retain(|c| seen_calls.insert(c.clone()));
        for m in &mut s.mentions {
            if let Some((_, exported)) = aliases.get(m.as_str()) {
                *m = exported.clone();
            }
        }
        let mut seen_mentions = HashSet::with_capacity(s.mentions.len());
        s.mentions.retain(|m| seen_mentions.insert(m.clone()));
    }
}
