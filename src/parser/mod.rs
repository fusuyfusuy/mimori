pub mod common;
pub mod go;
pub mod lang;
pub mod python;
pub mod rust;
pub mod typescript;

pub use common::ParseDiagnostics;
pub use lang::Language;

use crate::model::Symbol;
use crate::workspace::AliasSet;
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn parse_file(path: &Path, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let (symbols, diag) = parse_file_with_diagnostics(path, content, aliases)?;
    if diag.has_issues() {
        let mut reasons = Vec::new();
        if diag.has_error {
            reasons.push("syntax error");
        }
        if diag.depth_truncated {
            reasons.push("max AST depth exceeded");
        }
        eprintln!(
            "mimori: parse_error in {} ({})",
            path.display(),
            reasons.join(", ")
        );
    }
    Ok(symbols)
}

pub fn parse_file_with_diagnostics(
    path: &Path,
    content: &str,
    aliases: &AliasSet,
) -> Result<(Vec<Symbol>, ParseDiagnostics)> {
    let lang = Language::from_path(path);
    let Some(file_str) = path.to_str() else {
        bail!("Non-UTF8 file path cannot be indexed: {}", path.display());
    };

    match lang {
        Language::Rust => rust::parse_rust_with_diagnostics(file_str, content, aliases),
        Language::TypeScript | Language::JavaScript => {
            typescript::parse_typescript_with_diagnostics(file_str, content, false, aliases)
        }
        Language::TSX => {
            typescript::parse_typescript_with_diagnostics(file_str, content, true, aliases)
        }
        Language::Python => python::parse_python_with_diagnostics(file_str, content, aliases),
        Language::Go => go::parse_go_with_diagnostics(file_str, content, aliases),
        Language::Unknown => bail!("Unsupported language for file: {}", path.display()),
    }
}

const SCRIPT_EXTS: &[&str] = &[".tsx", ".mts", ".cts", ".jsx", ".mjs", ".cjs", ".ts", ".js"];

fn strip_alias_prefix<'a>(s: &'a str, aliases: &AliasSet) -> &'a str {
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
        // If `@scope/*` is a single-segment alias (e.g. `@app/*` -> `./src/*`),
        // strip only the first segment (`@app/utils/fmt` -> `utils/fmt`).
        if aliases.matches(&format!("{first}/_")) {
            return rest;
        }
        // Otherwise check for a two-segment monorepo/npm scope alias
        // (e.g. `@app/server/*` -> `./packages/server/src/*` or `@acme/core`).
        if let Some((pkg, after_pkg)) = rest.split_once('/') {
            if aliases.matches(&format!("{first}/{pkg}/_"))
                || aliases.matches(&format!("{first}/{pkg}"))
            {
                return after_pkg;
            }
            return rest;
        }
        // Bare `@scope/pkg` matching a workspace package root (barrel) has no file subpath.
        if aliases.matches(s) {
            return "";
        }
        return rest;
    }
    s
}

pub(crate) fn spec_to_module_hint(spec: &str, aliases: &AliasSet) -> String {
    let mut s = spec.trim();
    while let Some(rest) = s.strip_prefix("./").or_else(|| s.strip_prefix("../")) {
        s = rest;
    }
    s = strip_alias_prefix(s, aliases).trim_matches('/');
    strip_script_ext(s)
}

fn strip_script_ext(s: &str) -> String {
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
                let candidate = format_hinted_call(hint, exported);
                (candidate != *call).then_some(candidate)
            } else if let Some((head, tail)) = call.split_once("::") {
                // Only rewrite qualified `Head::tail` calls when `Head` is an explicit alias;
                // do not prepend `hint:` because graph resolution skips `:` hints on `::` paths.
                aliases
                    .get(head)
                    .filter(|(_, exported)| exported != head)
                    .map(|(_, exported)| format!("{exported}::{tail}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_alias_prefix_preserves_subpaths() {
        let empty = AliasSet::default();
        assert_eq!(spec_to_module_hint("@app/utils/fmt", &empty), "utils/fmt");
        assert_eq!(spec_to_module_hint("~/utils/fmt.ts", &empty), "utils/fmt");
        assert_eq!(spec_to_module_hint("./a/b.tsx", &empty), "a/b");

        let single = AliasSet::for_test(&["@app/*"]);
        assert_eq!(spec_to_module_hint("@app/utils/fmt", &single), "utils/fmt");
        let scoped = AliasSet::for_test(&["@app/server/*"]);
        assert_eq!(
            spec_to_module_hint("@app/server/services/permission", &scoped),
            "services/permission"
        );
    }
}
