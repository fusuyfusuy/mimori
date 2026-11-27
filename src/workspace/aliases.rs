//! Language-scoped workspace alias and package discovery (`tsconfig.json`,
//! `package.json`, `pnpm-workspace.yaml`, `Cargo.toml`, `go.mod`, Python local
//! modules) with strict workspace confinement before reading any file.

use crate::workspace::walker::{
    confine_to_workspace, fnv1a_hash, normalize_rel_str, read_bounded, read_to_string_bounded,
    MAX_INDEX_FILE_BYTES,
};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Hard cap on directories traversed while expanding workspace globs (`/**`).
const MAX_WORKSPACE_GLOB_DIRS: usize = 2_048;

/// One `compilerOptions.paths` key. TypeScript allows a single `*`, anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AliasPattern {
    prefix: String,
    suffix: String,
    wildcard: bool,
}

impl AliasPattern {
    fn matches(&self, spec: &str) -> bool {
        if !self.wildcard {
            return spec == self.prefix;
        }
        spec.len() >= self.prefix.len() + self.suffix.len()
            && spec.starts_with(&self.prefix)
            && spec.ends_with(&self.suffix)
    }
}

/// Workspace-owned path aliases and package namespaces, partitioned by
/// language so e.g. a Rust crate named `react` never pollutes TypeScript imports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasSet {
    patterns: Vec<AliasPattern>,
    js_packages: Vec<String>,
    rust_crates: Vec<String>,
    go_modules: Vec<String>,
    py_modules: Vec<String>,
    visited_configs: Vec<PathBuf>,
    fingerprint: String,
}

impl AliasSet {
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
            && self.js_packages.is_empty()
            && self.rust_crates.is_empty()
            && self.go_modules.is_empty()
            && self.py_modules.is_empty()
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// All confined manifest/config files and `baseUrl` directories consulted
    /// while building this `AliasSet`, sorted and deduplicated.
    pub fn visited_configs(&self) -> &[PathBuf] {
        &self.visited_configs
    }

    /// True when `spec` is a first-party JS/TS alias or workspace package.
    pub fn matches(&self, spec: &str) -> bool {
        if self.patterns.iter().any(|p| p.matches(spec)) {
            return true;
        }
        matches_pkg_list(&self.js_packages, spec)
    }

    /// True when `crate_name` belongs to a Rust crate declared in this workspace.
    pub fn matches_rust(&self, crate_name: &str) -> bool {
        self.rust_crates.iter().any(|c| c == crate_name)
    }

    /// True when `import_path` belongs to a Go module declared in workspace `go.mod`.
    pub fn matches_go(&self, import_path: &str) -> bool {
        matches_pkg_list(&self.go_modules, import_path)
    }

    pub fn has_go_modules(&self) -> bool {
        !self.go_modules.is_empty()
    }

    /// True when `top_module` matches a local Python module/package discovered in the workspace.
    pub fn matches_python(&self, top_module: &str) -> bool {
        self.py_modules.iter().any(|m| m == top_module)
    }

    pub fn has_python_modules(&self) -> bool {
        !self.py_modules.is_empty()
    }

    #[cfg(test)]
    pub fn for_test(keys: &[&str]) -> AliasSet {
        AliasSet {
            patterns: keys
                .iter()
                .filter_map(|k| pattern_for(k, &[format!("./{k}")]))
                .collect(),
            ..Default::default()
        }
    }

    pub fn collect(root: &Path) -> AliasSet {
        let discovery = super::walker::discover_workspace(root);
        Self::collect_with_discovery(root, &discovery)
    }

    pub fn collect_with_discovery(
        root: &Path,
        discovery: &super::walker::WorkspaceDiscovery,
    ) -> AliasSet {
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let (manifests, configs, workspace_dirs) =
            collect_js_manifests(&canon_root, root, discovery);

        let packages = read_package_names(&manifests);
        let mut set = AliasSet {
            js_packages: packages.iter().map(|(name, _)| name.clone()).collect(),
            ..Default::default()
        };
        add_workspace_dir_packages(&canon_root, &workspace_dirs, &mut set.js_packages);

        let confined_cargo = confine_discovered_list(&canon_root, root, &discovery.cargo_manifests);
        let confined_gomods = confine_discovered_list(&canon_root, root, &discovery.go_mods);

        read_cargo_package_names(&confined_cargo, &mut set.rust_crates);
        read_go_module_names(&confined_gomods, &mut set.go_modules);
        collect_python_modules(discovery, &mut set.py_modules);

        let mut visited_configs = HashSet::new();
        let mut watched_dirs = HashSet::new();
        for config in &configs {
            let mut paths = Vec::new();
            collect_paths(
                &canon_root,
                config,
                &packages,
                &mut visited_configs,
                &mut watched_dirs,
                &mut paths,
            );
            for (key, replacements) in paths {
                if let Some(pattern) = pattern_for(&key, &replacements) {
                    if !set.patterns.contains(&pattern) {
                        set.patterns.push(pattern);
                    }
                }
            }
        }

        let pnpm_yaml = confine_to_workspace(&canon_root, Path::new("pnpm-workspace.yaml")).ok();
        let mut all_files: Vec<PathBuf> = manifests
            .into_iter()
            .chain(confined_cargo)
            .chain(confined_gomods)
            .chain(visited_configs)
            .collect();
        all_files.sort();
        all_files.dedup();

        set.fingerprint =
            compute_alias_fingerprint(&canon_root, pnpm_yaml.as_deref(), &all_files, &set);

        let mut all_watched = all_files;
        if let Some(pnpm) = pnpm_yaml {
            all_watched.push(pnpm);
        }
        all_watched.extend(watched_dirs);
        all_watched.sort();
        all_watched.dedup();
        set.visited_configs = all_watched;
        set
    }
}

fn confine_discovered_list(canon_root: &Path, raw_root: &Path, paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|p| {
            let candidate = p.strip_prefix(raw_root).unwrap_or(p);
            confine_to_workspace(canon_root, candidate).ok()
        })
        .collect()
}

fn collect_js_manifests(
    canon_root: &Path,
    raw_root: &Path,
    discovery: &super::walker::WorkspaceDiscovery,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let mut manifests = confine_discovered_list(canon_root, raw_root, &discovery.package_manifests);
    let mut configs = confine_discovered_list(canon_root, raw_root, &discovery.tsconfigs);

    let mut workspace_globs = read_package_json_workspaces(canon_root);
    workspace_globs.extend(read_pnpm_workspaces(canon_root));
    let workspace_dirs = expand_workspace_globs(canon_root, &workspace_globs);

    for dir in &workspace_dirs {
        push_confined_file(canon_root, &dir.join("package.json"), &mut manifests);
        push_confined_file(canon_root, &dir.join("tsconfig.json"), &mut configs);
    }

    (manifests, configs, workspace_dirs)
}

fn push_confined_file(canon_root: &Path, candidate: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(confined) = confine_to_workspace(canon_root, candidate) {
        if confined.is_file() && !out.contains(&confined) {
            out.push(confined);
        }
    }
}

fn add_workspace_dir_packages(canon_root: &Path, dirs: &[PathBuf], js_packages: &mut Vec<String>) {
    for dir in dirs {
        let Ok(rel) = dir.strip_prefix(canon_root) else {
            continue;
        };
        let rel_str = normalize_rel_str(rel);
        if !rel_str.is_empty() && !js_packages.contains(&rel_str) {
            js_packages.push(rel_str);
        }
    }
}

fn compute_alias_fingerprint(
    canon_root: &Path,
    pnpm_yaml: Option<&Path>,
    all_files: &[PathBuf],
    set: &AliasSet,
) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mix = |h: u64, bytes: &[u8]| fnv1a_hash(bytes) ^ h.wrapping_mul(0x100000001b3);

    if let Some(pnpm) = pnpm_yaml {
        if let Ok(Some(content)) = read_bounded(pnpm, MAX_INDEX_FILE_BYTES) {
            hash = mix(hash, &content);
        }
    }

    for path in all_files {
        let rel = path.strip_prefix(canon_root).unwrap_or(path);
        hash = mix(hash, normalize_rel_str(rel).as_bytes());
        if let Ok(Some(content)) = read_bounded(path, MAX_INDEX_FILE_BYTES) {
            hash = mix(hash, &content);
        }
    }

    for list in [
        &set.js_packages,
        &set.rust_crates,
        &set.go_modules,
        &set.py_modules,
    ] {
        let mut sorted = list.clone();
        sorted.sort();
        sorted.dedup();
        hash = mix(hash, b"|");
        for pkg in &sorted {
            hash = mix(hash, pkg.as_bytes());
        }
    }

    let mut sorted_pats: Vec<(&str, &str, bool)> = set
        .patterns
        .iter()
        .map(|p| (p.prefix.as_str(), p.suffix.as_str(), p.wildcard))
        .collect();
    sorted_pats.sort_unstable();
    for (pref, suf, wc) in sorted_pats {
        hash = mix(hash, pref.as_bytes());
        hash = mix(hash, suf.as_bytes());
        hash = mix(hash, &[u8::from(wc)]);
    }

    format!("{hash:016x}")
}

fn matches_pkg_list(list: &[String], spec: &str) -> bool {
    list.iter().any(|p| {
        spec == p
            || (spec.len() > p.len()
                && spec.starts_with(p.as_str())
                && spec.as_bytes()[p.len()] == b'/')
    })
}

fn collect_python_modules(discovery: &super::walker::WorkspaceDiscovery, out: &mut Vec<String>) {
    let mut seen = HashSet::new();
    for (rel, _, _, _) in &discovery.supported {
        if !rel.ends_with(".py") {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if let Some(&first) = parts.first() {
            let stem = first.strip_suffix(".py").unwrap_or(first);
            if !stem.is_empty() && stem != "__init__" && seen.insert(stem.to_string()) {
                out.push(stem.to_string());
            }
        }
        // Also recognize `src/<pkg>/...` layout.
        if parts.len() >= 2 && matches!(parts[0], "src" | "lib" | "app") {
            let second = parts[1].strip_suffix(".py").unwrap_or(parts[1]);
            if !second.is_empty() && second != "__init__" && seen.insert(second.to_string()) {
                out.push(second.to_string());
            }
        }
    }
    out.sort();
}

fn read_package_json_workspaces(root: &Path) -> Vec<String> {
    let Ok(pkg_path) = confine_to_workspace(root, Path::new("package.json")) else {
        return Vec::new();
    };
    let Ok(Some(raw)) = read_to_string_bounded(&pkg_path, MAX_INDEX_FILE_BYTES) else {
        return Vec::new();
    };
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    let Ok(json) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(workspaces) = json.get("workspaces") {
        if let Some(arr) = workspaces.as_array() {
            for item in arr.iter().filter_map(Value::as_str) {
                out.push(item.to_string());
            }
        } else if let Some(obj) = workspaces.as_object() {
            if let Some(arr) = obj.get("packages").and_then(Value::as_array) {
                for item in arr.iter().filter_map(Value::as_str) {
                    out.push(item.to_string());
                }
            }
        }
    }
    out
}

fn read_pnpm_workspaces(root: &Path) -> Vec<String> {
    let Ok(yaml_path) = confine_to_workspace(root, Path::new("pnpm-workspace.yaml")) else {
        return Vec::new();
    };
    let Ok(Some(raw)) = read_to_string_bounded(&yaml_path, MAX_INDEX_FILE_BYTES) else {
        return Vec::new();
    };
    parse_yaml_packages_list(&raw)
}

fn strip_yaml_inline_comment(s: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return s[..i].trim_end(),
            _ => {}
        }
    }
    s.trim_end()
}

fn parse_yaml_packages_list(raw: &str) -> Vec<String> {
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let mut out = Vec::new();
    let mut in_packages = false;
    for line in raw.lines() {
        let cleaned = strip_yaml_inline_comment(line);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(after) = trimmed.strip_prefix("packages:") {
            let rest = after.trim();
            if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                for tok in inner.split(',') {
                    let clean = tok.trim().trim_matches(|c| c == '\'' || c == '"').trim();
                    if !clean.is_empty() {
                        out.push(clean.to_string());
                    }
                }
                in_packages = false;
            } else {
                in_packages = true;
            }
            continue;
        }
        if in_packages {
            if let Some(item) = trimmed.strip_prefix('-') {
                let clean = item.trim().trim_matches(|c| c == '\'' || c == '"').trim();
                if !clean.is_empty() {
                    out.push(clean.to_string());
                }
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                in_packages = false;
            }
        }
    }
    out
}

fn is_unsafe_glob_pattern(pat: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let normalized = pat.replace('\\', "/");
    if normalized.starts_with('/') || Path::new(pat).is_absolute() {
        return true;
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    normalized.split('/').any(|seg| seg == "..")
}

fn collect_dirs_recursive(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 || out.len() >= MAX_WORKSPACE_GLOB_DIRS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_WORKSPACE_GLOB_DIRS {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if matches!(
                name,
                "node_modules" | ".git" | "target" | "dist" | "build" | ".mimori"
            ) {
                continue;
            }
        }
        out.push(path.clone());
        collect_dirs_recursive(&path, depth + 1, out);
    }
}

fn expand_single_glob(root: &Path, clean: &str, out: &mut Vec<PathBuf>) {
    if is_unsafe_glob_pattern(clean) || out.len() >= MAX_WORKSPACE_GLOB_DIRS {
        return;
    }
    if let Some(parent) = clean.strip_suffix("/**") {
        let rel = if parent.is_empty() { "." } else { parent };
        if let Ok(confined_parent) = confine_to_workspace(root, Path::new(rel)) {
            if confined_parent.is_dir() {
                collect_dirs_recursive(&confined_parent, 0, out);
            }
        }
        return;
    }
    if let Some(parent) = clean.strip_suffix("/*") {
        let rel = if parent.is_empty() { "." } else { parent };
        let Ok(confined_parent) = confine_to_workspace(root, Path::new(rel)) else {
            return;
        };
        if let Ok(entries) = std::fs::read_dir(&confined_parent) {
            for entry in entries.flatten() {
                if out.len() >= MAX_WORKSPACE_GLOB_DIRS {
                    break;
                }
                if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    out.push(entry.path());
                }
            }
        }
        return;
    }
    if let Ok(confined_dir) = confine_to_workspace(root, Path::new(clean)) {
        if confined_dir.is_dir() {
            out.push(confined_dir);
        }
    }
}

fn expand_workspace_globs(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for pat in patterns {
        let trimmed = pat.trim();
        if let Some(neg) = trimmed.strip_prefix('!') {
            excludes.push(neg.trim_end_matches('/').to_string());
        } else {
            includes.push(trimmed.trim_end_matches('/').to_string());
        }
    }

    let mut package_dirs = Vec::new();
    for clean in &includes {
        expand_single_glob(root, clean, &mut package_dirs);
    }

    let canon_root = root.canonicalize().ok();
    let mut confined = Vec::new();
    for d in package_dirs {
        let Ok(cd) = confine_to_workspace(root, &d) else {
            continue;
        };
        let rel = canon_root
            .as_deref()
            .and_then(|cr| cd.strip_prefix(cr).ok())
            .map(normalize_rel_str)
            .unwrap_or_default();
        let excluded = excludes.iter().any(|ex| {
            if let Some(pref) = ex.strip_suffix("/*").or_else(|| ex.strip_suffix("/**")) {
                rel == pref || rel.starts_with(&format!("{pref}/"))
            } else {
                rel == *ex
            }
        });
        if !excluded {
            confined.push(cd);
        }
    }
    confined.sort();
    confined.dedup();
    confined
}

/// `package.json` `name` -> the directory that declares it.
fn read_package_names(manifests: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for path in manifests {
        let Some(dir) = path.parent() else {
            continue;
        };
        let Ok(Some(raw)) = read_to_string_bounded(path, MAX_INDEX_FILE_BYTES) else {
            continue;
        };
        let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        match serde_json::from_str::<Value>(raw) {
            Ok(json) => {
                if let Some(name) = json.get("name").and_then(Value::as_str) {
                    if !name.is_empty() {
                        out.push((name.to_string(), dir.to_path_buf()));
                    }
                }
            }
            Err(e) => eprintln!(
                "mimori: {} is unreadable ({e}); imports of it stay external",
                path.display()
            ),
        }
    }
    out.sort();
    out
}

fn push_rust_crate(out: &mut Vec<String>, name: &str) {
    if name.is_empty() {
        return;
    }
    if !out.iter().any(|p| p == name) {
        out.push(name.to_string());
    }
    let ident = name.replace('-', "_");
    if !out.iter().any(|p| p == &ident) {
        out.push(ident);
    }
}

fn read_cargo_package_names(cargo_manifests: &[PathBuf], out: &mut Vec<String>) {
    for cargo_toml in cargo_manifests {
        let Ok(Some(content)) = read_to_string_bounded(cargo_toml, MAX_INDEX_FILE_BYTES) else {
            continue;
        };
        let mut section = "";
        for line in content.lines() {
            let trimmed = line.split('#').next().unwrap_or("").trim();
            if trimmed.starts_with('[') {
                section = trimmed;
                continue;
            }
            if section != "[package]" && section != "[lib]" {
                continue;
            }
            let Some((key, val)) = trimmed.split_once('=') else {
                continue;
            };
            if key.trim() == "name" {
                let name = val.trim().trim_matches('"').trim_matches('\'').trim();
                push_rust_crate(out, name);
            }
        }
    }
}

fn read_go_module_names(go_mods: &[PathBuf], out: &mut Vec<String>) {
    for go_mod in go_mods {
        let Ok(Some(content)) = read_to_string_bounded(go_mod, MAX_INDEX_FILE_BYTES) else {
            continue;
        };
        for line in content.lines() {
            let trimmed = line.split("//").next().unwrap_or("").trim();
            let mut tokens = trimmed.split_whitespace();
            if tokens.next() != Some("module") {
                continue;
            }
            if let Some(raw_mod) = tokens.next() {
                let mod_name = raw_mod.trim_matches('"').trim();
                if !mod_name.is_empty() && !out.iter().any(|p| p == mod_name) {
                    out.push(mod_name.to_string());
                }
            }
        }
    }
}

/// Rejects wildcard `"*"` and aliases whose every replacement targets `node_modules`.
fn pattern_for(key: &str, replacements: &[String]) -> Option<AliasPattern> {
    if !replacements.is_empty() && replacements.iter().all(|r| r.contains("node_modules")) {
        return None;
    }
    match key.split_once('*') {
        Some((prefix, suffix)) => {
            if prefix.is_empty() {
                return None;
            }
            Some(AliasPattern {
                prefix: prefix.to_string(),
                suffix: suffix.to_string(),
                wildcard: true,
            })
        }
        None => Some(AliasPattern {
            prefix: key.to_string(),
            suffix: String::new(),
            wildcard: false,
        }),
    }
}

fn append_json_ext(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".json");
    PathBuf::from(os)
}

fn is_absolute_spec(spec: &str) -> bool {
    let norm = spec.replace('\\', "/");
    if norm.starts_with('/') || Path::new(spec).is_absolute() {
        return true;
    }
    let bytes = norm.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn resolve_tsconfig_target(root: &Path, base: &Path) -> Option<PathBuf> {
    if let Ok(confined) = confine_to_workspace(root, base) {
        if confined.is_file() {
            return Some(confined);
        }
        if confined.is_dir() {
            let nested = confined.join("tsconfig.json");
            if let Ok(nested_confined) = confine_to_workspace(root, &nested) {
                if nested_confined.is_file() {
                    return Some(nested_confined);
                }
            }
        }
    }
    let with_json = append_json_ext(base);
    if let Ok(confined_json) = confine_to_workspace(root, &with_json) {
        if confined_json.is_file() {
            return Some(confined_json);
        }
    }
    None
}

fn collect_paths(
    root: &Path,
    config: &Path,
    packages: &[(String, PathBuf)],
    visited: &mut HashSet<PathBuf>,
    watched_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    let Ok(normalized) = confine_to_workspace(root, config) else {
        return;
    };
    if !visited.insert(normalized.clone()) {
        return;
    }

    let Ok(Some(raw)) = read_to_string_bounded(&normalized, MAX_INDEX_FILE_BYTES) else {
        return;
    };
    let Some(json) = parse_jsonc(&raw) else {
        eprintln!(
            "mimori: {} could not be parsed; its path aliases are not applied",
            config.display()
        );
        return;
    };

    match json.get("extends") {
        Some(Value::String(parent)) => follow_extends(
            root,
            &normalized,
            parent,
            packages,
            visited,
            watched_dirs,
            out,
        ),
        Some(Value::Array(parents)) => {
            for parent in parents.iter().filter_map(Value::as_str) {
                follow_extends(
                    root,
                    &normalized,
                    parent,
                    packages,
                    visited,
                    watched_dirs,
                    out,
                );
            }
        }
        _ => {}
    }

    if let Some(refs) = json.get("references").and_then(Value::as_array) {
        for r in refs {
            let ref_path = r.as_str().or_else(|| r.get("path").and_then(Value::as_str));
            if let Some(target_spec) = ref_path {
                follow_reference(
                    root,
                    &normalized,
                    target_spec,
                    packages,
                    visited,
                    watched_dirs,
                    out,
                );
            }
        }
    }

    if let Some(base_url) = json
        .get("compilerOptions")
        .and_then(|c| c.get("baseUrl"))
        .and_then(Value::as_str)
    {
        collect_base_url_aliases(root, &normalized, base_url, watched_dirs, out);
    }

    let Some(paths) = json
        .get("compilerOptions")
        .and_then(|c| c.get("paths"))
        .and_then(Value::as_object)
    else {
        return;
    };
    for (key, replacements) in paths {
        let values: Vec<String> = match replacements {
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        out.push((key.clone(), values));
    }
}

fn follow_extends(
    root: &Path,
    config: &Path,
    parent: &str,
    packages: &[(String, PathBuf)],
    visited: &mut HashSet<PathBuf>,
    watched_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    if is_absolute_spec(parent) {
        return;
    }
    let base = if parent.starts_with('.') {
        config.parent().unwrap_or(Path::new("")).join(parent)
    } else {
        let Some((name, rest)) = split_package_spec(parent) else {
            return;
        };
        let Some((_, dir)) = packages.iter().find(|(n, _)| *n == name) else {
            return;
        };
        dir.join(rest)
    };
    let Some(target) = resolve_tsconfig_target(root, &base) else {
        return;
    };
    collect_paths(root, &target, packages, visited, watched_dirs, out);
}

fn follow_reference(
    root: &Path,
    config: &Path,
    target_spec: &str,
    packages: &[(String, PathBuf)],
    visited: &mut HashSet<PathBuf>,
    watched_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    if is_absolute_spec(target_spec) {
        return;
    }
    let config_dir = config.parent().unwrap_or(Path::new(""));
    let candidate = if target_spec.starts_with('.') {
        config_dir.join(target_spec)
    } else if let Some((name, rest)) = split_package_spec(target_spec) {
        if let Some((_, dir)) = packages.iter().find(|(n, _)| *n == name) {
            dir.join(rest)
        } else {
            config_dir.join(target_spec)
        }
    } else if let Some((_, dir)) = packages.iter().find(|(n, _)| *n == target_spec) {
        dir.clone()
    } else {
        config_dir.join(target_spec)
    };

    let Some(target) = resolve_tsconfig_target(root, &candidate) else {
        return;
    };
    collect_paths(root, &target, packages, visited, watched_dirs, out);
}

fn collect_base_url_aliases(
    root: &Path,
    config: &Path,
    base_url: &str,
    watched_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    if is_absolute_spec(base_url) {
        return;
    }
    let config_dir = config.parent().unwrap_or(Path::new(""));
    let base_dir = config_dir.join(base_url);
    let Ok(canon_base_dir) = confine_to_workspace(root, &base_dir) else {
        return;
    };
    watched_dirs.insert(canon_base_dir.clone());
    let Ok(entries) = std::fs::read_dir(&canon_base_dir) else {
        return;
    };
    let is_root_like = base_url == "." || base_url == "./";
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.')
            || [
                "node_modules",
                "target",
                "dist",
                "build",
                "vendor",
                ".git",
                ".mimori",
            ]
            .contains(&name)
        {
            continue;
        }
        if is_root_like && !["src", "lib", "app", "source"].contains(&name) {
            continue;
        }
        if path.is_dir() {
            out.push((format!("{name}/*"), vec![format!("./{name}/*")]));
            out.push((name.to_string(), vec![format!("./{name}")]));
        } else if path.is_file() {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ["ts", "tsx", "js", "jsx"].contains(&ext) {
                    out.push((stem.to_string(), vec![format!("./{stem}")]));
                }
            }
        }
    }
}

fn split_package_spec(spec: &str) -> Option<(String, String)> {
    let split_at = if spec.starts_with('@') {
        spec.match_indices('/').nth(1).map(|(i, _)| i)
    } else {
        spec.find('/')
    }?;
    let name = &spec[..split_at];
    let rest = spec[split_at + 1..].trim_start_matches('/');
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    Some((name.to_string(), rest.to_string()))
}

fn parse_jsonc(raw: &str) -> Option<Value> {
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    serde_json::from_str(&strip_trailing_commas(&strip_comments(raw))).ok()
}

fn strip_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_trailing_commas(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut look = chars.clone();
            let mut next_sig = None;
            for lc in look.by_ref() {
                if !lc.is_whitespace() {
                    next_sig = Some(lc);
                    break;
                }
            }
            if matches!(next_sig, Some(']') | Some('}')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_and_exact_patterns_match_their_own_specifiers_only() {
        let set = AliasSet::for_test(&["@/*", "@app/server/*", "exact-alias"]);
        assert!(set.matches("@/utils/date"));
        assert!(set.matches("@app/server/db"));
        assert!(set.matches("exact-alias"));
        assert!(!set.matches("react"));
        assert!(!set.matches("exact-alias/sub"));
    }

    #[test]
    fn jsonc_strips_bom_comments_and_trailing_commas() {
        let raw = "\u{FEFF}{\n  // comment\n  \"compilerOptions\": { /* block */ \"paths\": { \"@/*\": [\"./src/*\",], }, }\n}";
        assert!(parse_jsonc(raw).is_some());
    }

    #[test]
    fn node_modules_fallback_array_preserves_first_party_alias() {
        assert!(pattern_for("@d/*", &["./src/*".into(), "./node_modules/@d/*".into()]).is_some());
        assert!(pattern_for("lodash", &["./node_modules/lodash".into()]).is_none());
    }

    #[test]
    fn yaml_packages_handles_inline_comments_and_flow_lists() {
        let y1 = "packages:\n  - 'packages/*' # inline comment\n  - '!packages/ignored'\n";
        assert_eq!(
            parse_yaml_packages_list(y1),
            vec!["packages/*", "!packages/ignored"]
        );
        let y2 = "packages: ['apps/*', 'libs/*'] # flow\n";
        assert_eq!(parse_yaml_packages_list(y2), vec!["apps/*", "libs/*"]);
    }
}
