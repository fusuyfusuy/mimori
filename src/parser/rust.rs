use crate::model::{symbol_flags, Symbol, SymbolKind};
use crate::parser::common::{
    build_symbol, extract_signature_ast, is_valid_ident, node_text, ImportSet, ParseDiagnostics,
    RefCollector, ScopeStack, MAX_AST_DEPTH,
};
use crate::workspace::AliasSet;
use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tree_sitter::{Node, Parser};

thread_local! {
    static RUST_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("Error loading Rust grammar");
        p
    });
}

struct RustWalkCtx<'a> {
    content: &'a str,
    file: &'a str,
    file_is_test: bool,
    external_set: &'a HashSet<String>,
    serde_targets: &'a HashSet<String>,
    outline_mods: &'a HashSet<String>,
    depth_truncated: Cell<bool>,
}

impl<'a> RustWalkCtx<'a> {
    fn check_depth(&self, depth: usize) -> bool {
        if depth >= MAX_AST_DEPTH {
            self.depth_truncated.set(true);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Default)]
struct RustWalkState<'a> {
    parent_type: Option<&'a str>,
    in_test: bool,
    in_trait_impl: bool,
    in_fn: bool,
    in_serde_mod: bool,
}

pub fn parse_rust(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    Ok(parse_rust_with_diagnostics(file, content, aliases)?.0)
}

pub fn parse_rust_with_diagnostics(
    file: &str,
    content: &str,
    aliases: &AliasSet,
) -> Result<(Vec<Symbol>, ParseDiagnostics)> {
    let tree = RUST_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;

    let root = tree.root_node();
    let depth_truncated = Cell::new(false);
    let (local_mods, outline_mods) = collect_declared_mods(root, content, &depth_truncated);
    let external_list =
        collect_file_external_imports(root, content, aliases, &local_mods, &depth_truncated);
    let external_set: HashSet<String> = external_list.iter().cloned().collect();
    let serde_targets = collect_serde_hook_targets(root, content, &depth_truncated);

    let file_is_test = file.starts_with("tests/")
        || file.contains("/tests/")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs");

    let ctx = RustWalkCtx {
        content,
        file,
        file_is_test,
        external_set: &external_set,
        serde_targets: &serde_targets,
        outline_mods: &outline_mods,
        depth_truncated: Cell::new(depth_truncated.get()),
    };

    let mut symbols = Vec::new();
    let initial = RustWalkState {
        in_test: file_is_test,
        ..Default::default()
    };
    walk_rust_children(root, &ctx, initial, &mut symbols, 0);

    let mut import_aliases = HashMap::new();
    collect_local_use_aliases(
        root,
        content,
        &mut import_aliases,
        aliases,
        &local_mods,
        &ctx.depth_truncated,
        0,
    );
    super::apply_import_aliases(&mut symbols, &import_aliases);

    let external_imports: Arc<[String]> = external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    let diag = ParseDiagnostics {
        has_error: root.has_error(),
        depth_truncated: ctx.depth_truncated.get(),
    };
    Ok((symbols, diag))
}

/// Linear forward sibling walk that accumulates `attribute_item` slices without
/// backward `prev_sibling()` scans (prevents O(N^3) blowup on comment/attr runs)
/// and applies inner `#![...]` attributes to the enclosing block/module scope.
fn walk_rust_children<'a>(
    parent: Node<'a>,
    ctx: &RustWalkCtx<'a>,
    mut state: RustWalkState<'a>,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let content = ctx.content;
    let mut pending_attrs: Vec<&'a str> = Vec::new();
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            "attribute_item" => {
                pending_attrs.push(node_text(child, content));
            }
            "inner_attribute_item" => {
                let raw = node_text(child, content);
                if attr_marks_test(raw) {
                    state.in_test = true;
                }
                if attr_marks_entry(raw) {
                    state.in_serde_mod = true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => {
                walk_rust_node(child, ctx, state, &pending_attrs, symbols, depth + 1);
                pending_attrs.clear();
            }
        }
    }
}

fn walk_rust_node<'a>(
    node: Node<'a>,
    ctx: &RustWalkCtx<'a>,
    state: RustWalkState<'a>,
    attrs: &[&'a str],
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let kind = node.kind();
    let content = ctx.content;
    let node_is_test = state.in_test || ctx.file_is_test || attrs_mark_test(attrs);
    let node_is_attr_entry = state.in_serde_mod || attrs_mark_entry(attrs);

    match kind {
        "mod_item" => {
            let mod_name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, content).trim())
                .unwrap_or("");
            let next = RustWalkState {
                parent_type: None,
                in_test: node_is_test,
                in_trait_impl: false,
                in_fn: false,
                in_serde_mod: state.in_serde_mod || ctx.serde_targets.contains(mod_name),
            };
            if let Some(body) = node.child_by_field_name("body") {
                walk_rust_children(body, ctx, next, symbols, depth + 1);
            }
            return;
        }
        "function_item" => {
            emit_named_rust_item(
                node,
                ctx,
                state.parent_type,
                if state.parent_type.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                },
                node_is_test,
                state.in_trait_impl,
                node_is_attr_entry,
                None,
                symbols,
            );
            let next = RustWalkState {
                parent_type: None,
                in_test: node_is_test,
                in_trait_impl: false,
                in_fn: true,
                in_serde_mod: state.in_serde_mod,
            };
            if let Some(body) = node.child_by_field_name("body") {
                walk_rust_children(body, ctx, next, symbols, depth + 1);
            }
            return;
        }
        "function_signature_item" => {
            emit_named_rust_item(
                node,
                ctx,
                state.parent_type,
                SymbolKind::Method,
                node_is_test,
                true,
                node_is_attr_entry,
                None,
                symbols,
            );
            return;
        }
        "struct_item" | "enum_item" => {
            let sym_kind = if kind == "struct_item" {
                SymbolKind::Struct
            } else {
                SymbolKind::Enum
            };
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = compute_flags(node, name, ctx, node_is_test, false, node_is_attr_entry);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    sym_kind,
                    Some(name),
                    flags,
                    None,
                ));
            }
            return;
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = compute_flags(node, name, ctx, node_is_test, false, node_is_attr_entry);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Trait,
                    Some(name),
                    flags,
                    None,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    let next = RustWalkState {
                        parent_type: Some(name),
                        in_test: node_is_test,
                        in_trait_impl: true,
                        in_fn: false,
                        in_serde_mod: state.in_serde_mod,
                    };
                    walk_rust_children(body, ctx, next, symbols, depth + 1);
                }
                return;
            }
        }
        "impl_item" => {
            walk_rust_impl(
                node,
                ctx,
                node_is_test,
                node_is_attr_entry,
                state,
                symbols,
                depth,
            );
            return;
        }
        "type_item" => {
            emit_named_rust_item(
                node,
                ctx,
                state.parent_type,
                SymbolKind::TypeAlias,
                node_is_test,
                state.in_trait_impl,
                node_is_attr_entry,
                None,
                symbols,
            );
            return;
        }
        "const_item" | "static_item" if !state.in_fn => {
            emit_named_rust_item(
                node,
                ctx,
                state.parent_type,
                SymbolKind::Constant,
                node_is_test,
                state.in_trait_impl,
                node_is_attr_entry,
                None,
                symbols,
            );
            return;
        }
        _ => {}
    }

    let next = RustWalkState {
        in_test: node_is_test,
        ..state
    };
    walk_rust_children(node, ctx, next, symbols, depth + 1);
}

#[allow(clippy::too_many_arguments)]
fn emit_named_rust_item(
    node: Node,
    ctx: &RustWalkCtx<'_>,
    parent_type: Option<&str>,
    sym_kind: SymbolKind,
    is_test: bool,
    is_trait_impl: bool,
    is_attr_entry: bool,
    impl_trait_mention: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(name_node, ctx.content).trim();
    let full_name = match parent_type {
        Some(pt) => format!("{pt}::{name}"),
        None => name.to_string(),
    };
    let flags = compute_flags(node, name, ctx, is_test, is_trait_impl, is_attr_entry);
    symbols.push(create_symbol(
        node,
        ctx,
        full_name,
        sym_kind,
        parent_type,
        flags,
        impl_trait_mention,
    ));
}

fn walk_rust_impl<'a>(
    node: Node<'a>,
    ctx: &RustWalkCtx<'a>,
    node_is_test: bool,
    node_is_attr_entry: bool,
    state: RustWalkState<'a>,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    let content = ctx.content;
    let type_name = extract_impl_type_name(node, content);
    let trait_name = extract_impl_trait_name(node, content);
    let is_trait_impl = trait_name.is_some();

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut impl_in_test = node_is_test;
    let mut impl_in_entry = node_is_attr_entry;
    let mut pending_attrs: Vec<&'a str> = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "attribute_item" => {
                pending_attrs.push(node_text(child, content));
            }
            "inner_attribute_item" => {
                let raw = node_text(child, content);
                if attr_marks_test(raw) {
                    impl_in_test = true;
                }
                if attr_marks_entry(raw) {
                    impl_in_entry = true;
                }
            }
            "line_comment" | "block_comment" => {}
            "function_item" if trait_name.is_some() => {
                let child_test = impl_in_test || attrs_mark_test(&pending_attrs);
                let child_entry = impl_in_entry || attrs_mark_entry(&pending_attrs);
                emit_named_rust_item(
                    child,
                    ctx,
                    type_name,
                    SymbolKind::Method,
                    child_test,
                    true,
                    child_entry,
                    trait_name,
                    symbols,
                );
                if let Some(fn_body) = child.child_by_field_name("body") {
                    let next = RustWalkState {
                        parent_type: None,
                        in_test: child_test,
                        in_trait_impl: false,
                        in_fn: true,
                        in_serde_mod: state.in_serde_mod,
                    };
                    walk_rust_children(fn_body, ctx, next, symbols, depth + 2);
                }
                pending_attrs.clear();
            }
            _ => {
                let next = RustWalkState {
                    parent_type: type_name,
                    in_test: impl_in_test,
                    in_trait_impl: is_trait_impl,
                    in_fn: false,
                    in_serde_mod: state.in_serde_mod || impl_in_entry,
                };
                walk_rust_node(child, ctx, next, &pending_attrs, symbols, depth + 1);
                pending_attrs.clear();
            }
        }
    }
}

fn compute_flags(
    node: Node,
    name: &str,
    ctx: &RustWalkCtx<'_>,
    is_test: bool,
    is_trait_impl: bool,
    is_attr_entry: bool,
) -> u8 {
    let mut flags = 0u8;
    if is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if has_pub_visibility(node, ctx.content) {
        flags |= symbol_flags::IS_PUBLIC;
    }
    if is_trait_impl {
        flags |= symbol_flags::IS_TRAIT_IMPL;
    }
    if is_attr_entry || ctx.serde_targets.contains(name) {
        flags |= symbol_flags::IS_ATTR_ENTRY;
    }
    flags
}

/// Only unrestricted `pub` marks a Rust item `IS_PUBLIC`.
/// Scoped modifiers (`pub(crate)`, `pub(super)`, `pub(in ...)`) remain crate-internal.
fn has_pub_visibility(node: Node, content: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return node_text(child, content).trim() == "pub";
        }
    }
    false
}

fn strip_attr_brackets(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("#![")
        .or_else(|| trimmed.strip_prefix("#["))?
        .strip_suffix(']')?
        .trim();
    Some(inner)
}

fn split_attr_path_and_args(inner: &str) -> (&str, Option<&str>) {
    if let Some(open) = inner.find('(') {
        let path = inner[..open].trim();
        let rest = inner[open + 1..].trim();
        let args = rest.strip_suffix(')').map(str::trim);
        (path, args)
    } else if let Some(eq) = inner.find('=') {
        (inner[..eq].trim(), Some(inner[eq + 1..].trim()))
    } else {
        (inner.trim(), None)
    }
}

fn split_top_level_comma(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn cfg_expr_is_test(expr: &str) -> bool {
    let e = expr.trim();
    if e == "test" {
        return true;
    }
    if e.starts_with("not(") && e.ends_with(')') {
        return false;
    }
    if let Some(inner) = e
        .strip_prefix("all(")
        .or_else(|| e.strip_prefix("any("))
        .and_then(|s| s.strip_suffix(')'))
    {
        return split_top_level_comma(inner)
            .into_iter()
            .any(cfg_expr_is_test);
    }
    false
}

fn attr_marks_test(raw: &str) -> bool {
    let Some(inner) = strip_attr_brackets(raw) else {
        return false;
    };
    let (path, args) = split_attr_path_and_args(inner);
    let compact_path: String = path.chars().filter(|c| !c.is_whitespace()).collect();
    if compact_path == "test"
        || compact_path.ends_with("::test")
        || compact_path == "rstest"
        || compact_path.ends_with("::rstest")
        || compact_path == "test_case"
        || compact_path.ends_with("::test_case")
    {
        return true;
    }
    if compact_path == "cfg" || compact_path == "cfg_attr" {
        if let Some(a) = args {
            let cond = if compact_path == "cfg_attr" {
                split_top_level_comma(a).into_iter().next().unwrap_or("")
            } else {
                a
            };
            return cfg_expr_is_test(cond);
        }
    }
    false
}

fn attrs_mark_test(attrs: &[&str]) -> bool {
    attrs.iter().any(|raw| attr_marks_test(raw))
}

fn attr_marks_entry(raw: &str) -> bool {
    let Some(inner) = strip_attr_brackets(raw) else {
        return false;
    };
    let (path, args) = split_attr_path_and_args(inner);
    let compact_path: String = path.chars().filter(|c| !c.is_whitespace()).collect();
    if matches!(
        compact_path.as_str(),
        "no_mangle" | "export_name" | "proc_macro" | "proc_macro_derive" | "proc_macro_attribute"
    ) || compact_path.ends_with("::main")
        || compact_path.ends_with("::no_mangle")
        || compact_path.ends_with("::export_name")
        || compact_path.ends_with("::proc_macro")
        || compact_path.ends_with("::proc_macro_derive")
        || compact_path.ends_with("::proc_macro_attribute")
    {
        return true;
    }
    if compact_path == "unsafe" {
        if let Some(a) = args {
            let (inner_path, _) = split_attr_path_and_args(a);
            if matches!(inner_path.trim(), "no_mangle" | "export_name") {
                return true;
            }
        }
    }
    (compact_path == "serde" || compact_path.ends_with("::serde")) && args.is_some()
}

fn attrs_mark_entry(attrs: &[&str]) -> bool {
    attrs.iter().any(|raw| attr_marks_entry(raw))
}

const SERDE_CALLBACK_KEYS: &[&str] = &[
    "default",
    "with",
    "serialize_with",
    "deserialize_with",
    "skip_serializing_if",
    "from",
    "try_from",
    "into",
    "remote",
];

/// Scan file-level `#[serde(...)]` attributes using Tree-sitter token streams
/// (immune to raw-string literal injection) so referenced callback hooks and
/// modules receive `IS_ATTR_ENTRY`.
fn collect_serde_hook_targets(
    root: Node,
    content: &str,
    depth_truncated: &Cell<bool>,
) -> HashSet<String> {
    let mut targets = HashSet::new();
    walk_serde_attrs(root, content, &mut targets, depth_truncated, 0);
    targets
}

fn walk_serde_attrs(
    node: Node,
    content: &str,
    out: &mut HashSet<String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    if matches!(node.kind(), "attribute_item" | "inner_attribute_item") {
        extract_serde_attr_targets(node, content, out);
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_serde_attrs(c, content, out, depth_truncated, depth + 1);
    }
}

fn extract_serde_attr_targets(attr_item: Node, content: &str, out: &mut HashSet<String>) {
    let mut cursor = attr_item.walk();
    let Some(attr) = attr_item
        .children(&mut cursor)
        .find(|c| c.kind() == "attribute")
    else {
        return;
    };
    let Some(path_node) = attr.named_child(0) else {
        return;
    };
    let path = node_text(path_node, content).trim();
    if path != "serde" && !path.ends_with("::serde") {
        return;
    }
    let Some(args) = attr.child_by_field_name("arguments") else {
        return;
    };
    let mut tcursor = args.walk();
    let tokens: Vec<Node> = args.children(&mut tcursor).collect();
    let mut i = 0;
    while i + 2 < tokens.len() {
        let key_node = tokens[i];
        let eq_node = tokens[i + 1];
        let val_node = tokens[i + 2];
        if key_node.kind() == "identifier"
            && eq_node.kind() == "="
            && matches!(val_node.kind(), "string_literal" | "raw_string_literal")
            && SERDE_CALLBACK_KEYS.contains(&node_text(key_node, content).trim())
        {
            if let Some(unquoted) = unquote_rust_string(node_text(val_node, content)) {
                if let Some(last) = unquoted.trim().rsplit("::").next() {
                    if is_valid_ident(last) {
                        out.insert(last.to_string());
                    }
                }
            }
            i += 3;
            continue;
        }
        i += 1;
    }
}

fn unquote_rust_string(raw: &str) -> Option<&str> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return Some(rest);
    }
    let rest = s.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    let body = rest.get(hashes..)?.strip_prefix('"')?;
    let end = body.len().checked_sub(hashes + 1)?;
    if body.as_bytes().get(end) == Some(&b'"') && body[end + 1..].bytes().all(|b| b == b'#') {
        return Some(&body[..end]);
    }
    None
}

/// Collect `mod <name>;` and inline `mod <name> { ... }` items declared in this file.
/// Returns `(all_mods, outline_mods)` where `outline_mods` excludes inline `{ ... }` modules.
fn collect_declared_mods(
    root: Node,
    content: &str,
    depth_truncated: &Cell<bool>,
) -> (HashSet<String>, HashSet<String>) {
    let mut all_mods = HashSet::new();
    let mut outline_mods = HashSet::new();
    walk_declared_mods(
        root,
        content,
        &mut all_mods,
        &mut outline_mods,
        depth_truncated,
        0,
    );
    (all_mods, outline_mods)
}

fn walk_declared_mods(
    node: Node,
    content: &str,
    all_mods: &mut HashSet<String>,
    outline_mods: &mut HashSet<String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    if node.kind() == "mod_item" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(name_node, content).trim();
            if is_valid_ident(name) {
                all_mods.insert(name.to_string());
                if node.child_by_field_name("body").is_none() {
                    outline_mods.insert(name.to_string());
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_declared_mods(
            c,
            content,
            all_mods,
            outline_mods,
            depth_truncated,
            depth + 1,
        );
    }
}

fn extract_impl_trait_name<'a>(impl_node: Node, content: &'a str) -> Option<&'a str> {
    let trait_node = impl_node.child_by_field_name("trait")?;
    extract_simple_type_ident(trait_node, content)
}

fn extract_impl_type_name<'a>(impl_node: Node, content: &'a str) -> Option<&'a str> {
    let cur = impl_node.child_by_field_name("type")?;
    extract_simple_type_ident(cur, content)
}

fn extract_simple_type_ident<'a>(mut cur: Node, content: &'a str) -> Option<&'a str> {
    for _ in 0..16 {
        match cur.kind() {
            "generic_type" | "reference_type" => {
                cur = cur.child_by_field_name("type")?;
            }
            "pointer_type" => {
                cur = cur.child_by_field_name("type").or_else(|| {
                    (0..cur.named_child_count())
                        .rev()
                        .find_map(|i| cur.named_child(i))
                })?;
            }
            "scoped_type_identifier" => {
                cur = cur.child_by_field_name("name")?;
            }
            _ => break,
        }
    }
    let name = node_text(cur, content).trim();
    (!name.is_empty()).then_some(name)
}

fn init_fn_param_scope(fn_node: Node, content: &str, enclosing_type: Option<&str>) -> ScopeStack {
    let mut scopes = ScopeStack::new();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for p in params.children(&mut cursor) {
            if p.kind() == "parameter" {
                let pat = p
                    .child_by_field_name("pattern")
                    .and_then(|n| extract_ident_pattern(n, content));
                let ty = p
                    .child_by_field_name("type")
                    .and_then(|t| unwrap_receiver_type(t, content, enclosing_type));
                if let (Some(var), Some(ty_name)) = (pat, ty) {
                    scopes.insert(var.to_string(), ty_name);
                }
            }
        }
    }
    scopes
}

fn bind_closure_params(
    closure_node: Node,
    content: &str,
    enclosing_type: Option<&str>,
    scopes: &mut ScopeStack,
) {
    let Some(params) = closure_node.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = params.walk();
    for p in params.named_children(&mut cursor) {
        if p.kind() == "parameter" {
            if let Some(var) = p
                .child_by_field_name("pattern")
                .and_then(|n| extract_ident_pattern(n, content))
            {
                if let Some(ty) = p
                    .child_by_field_name("type")
                    .and_then(|t| unwrap_receiver_type(t, content, enclosing_type))
                {
                    scopes.insert(var.to_string(), ty);
                } else {
                    scopes.shadow(var.to_string());
                }
            }
        } else if let Some(var) = extract_ident_pattern(p, content) {
            scopes.shadow(var.to_string());
        }
    }
}

fn extract_ident_pattern<'a>(mut node: Node, content: &'a str) -> Option<&'a str> {
    for _ in 0..4 {
        match node.kind() {
            "identifier" => {
                let s = node_text(node, content).trim();
                return is_valid_ident(s).then_some(s);
            }
            "mut_pattern" | "ref_pattern" => {
                node = node.named_child(0)?;
            }
            _ => return None,
        }
    }
    None
}

fn unwrap_receiver_type(
    mut node: Node,
    content: &str,
    enclosing_type: Option<&str>,
) -> Option<String> {
    for _ in 0..8 {
        match node.kind() {
            "reference_type" => {
                node = node.child_by_field_name("type")?;
            }
            "generic_type" => {
                let base = node
                    .child_by_field_name("type")
                    .and_then(|t| extract_simple_type_ident(t, content));
                if matches!(
                    base,
                    Some("Box" | "Arc" | "Rc" | "Ref" | "RefMut" | "MutexGuard")
                ) {
                    if let Some(args) = node.child_by_field_name("type_arguments") {
                        if let Some(first_arg) = args.named_child(0) {
                            node = first_arg;
                            continue;
                        }
                    }
                }
                let ty = base?;
                if ty == "Self" {
                    return enclosing_type.map(str::to_string);
                }
                return is_user_type_name(ty).then(|| ty.to_string());
            }
            "type_identifier" | "scoped_type_identifier" => {
                let ty = extract_simple_type_ident(node, content)?;
                if ty == "Self" {
                    return enclosing_type.map(str::to_string);
                }
                return is_user_type_name(ty).then(|| ty.to_string());
            }
            _ => return None,
        }
    }
    None
}

fn is_user_type_name(ty: &str) -> bool {
    ty.starts_with(|c: char| c.is_ascii_uppercase())
        && !matches!(
            ty,
            "String"
                | "Vec"
                | "Option"
                | "Result"
                | "HashMap"
                | "BTreeMap"
                | "HashSet"
                | "BTreeSet"
                | "Path"
                | "PathBuf"
                | "OsStr"
                | "OsString"
                | "Box"
                | "Arc"
                | "Rc"
                | "Cow"
                | "File"
                | "Value"
                | "Error"
                | "Command"
        )
}

fn is_constructor_fn(name: &str) -> bool {
    matches!(name, "new" | "default" | "from" | "try_from" | "builder")
        || name.starts_with("with_")
        || name.starts_with("open")
        || name.starts_with("load")
        || name.starts_with("parse")
}

fn infer_expr_type(mut expr: Node, content: &str, enclosing_type: Option<&str>) -> Option<String> {
    for _ in 0..6 {
        match expr.kind() {
            "try_expression" | "await_expression" | "parenthesized_expression" => {
                expr = expr.named_child(0)?;
            }
            "reference_expression" => {
                expr = expr.child_by_field_name("value")?;
            }
            "call_expression" => {
                let mut fn_node = expr.child_by_field_name("function")?;
                while fn_node.kind() == "generic_function" {
                    fn_node = fn_node.child_by_field_name("function")?;
                }
                if fn_node.kind() == "scoped_identifier" {
                    let fn_name = fn_node
                        .child_by_field_name("name")
                        .map(|n| node_text(n, content).trim())
                        .unwrap_or("");
                    if !is_constructor_fn(fn_name) {
                        return None;
                    }
                    let path = fn_node.child_by_field_name("path")?;
                    let ty = extract_scoped_type(path, content, enclosing_type)?;
                    return is_user_type_name(ty).then(|| ty.to_string());
                }
                return None;
            }
            "struct_expression" => {
                let name = expr.child_by_field_name("name")?;
                let ty = extract_simple_type_ident(name, content)?;
                return is_user_type_name(ty).then(|| ty.to_string());
            }
            _ => return None,
        }
    }
    None
}

fn create_symbol(
    node: Node,
    ctx: &RustWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    enclosing_type: Option<&str>,
    flags: u8,
    impl_trait_mention: Option<&str>,
) -> Symbol {
    let content = ctx.content;
    let raw_body = node_text(node, content);
    let first_line = raw_body.lines().next().unwrap_or("").trim();
    let signature = if matches!(kind, SymbolKind::Constant | SymbolKind::Variable) {
        first_line.to_string()
    } else {
        extract_signature_ast(node, content, first_line)
    };

    let mut scopes = if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        init_fn_param_scope(node, content, enclosing_type)
    } else {
        ScopeStack::new()
    };

    let mut refs = RefCollector::default();
    if let Some(tr) = impl_trait_mention {
        refs.push_mention(tr);
    }
    collect_references(node, ctx, enclosing_type, &mut scopes, &mut refs, 0);

    build_symbol(node, ctx.file, name, kind, signature, refs, flags)
}

fn collect_references(
    node: Node,
    ctx: &RustWalkCtx<'_>,
    enclosing_type: Option<&str>,
    scopes: &mut ScopeStack,
    refs: &mut RefCollector,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let content = ctx.content;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if matches!(
            kind,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "impl_item"
                | "const_item"
                | "static_item"
                | "type_item"
                | "mod_item"
        ) {
            continue;
        }
        match kind {
            "block" | "for_expression" | "match_arm" => {
                scopes.push();
                collect_references(child, ctx, enclosing_type, scopes, refs, depth + 1);
                scopes.pop();
                continue;
            }
            "closure_expression" => {
                scopes.push();
                bind_closure_params(child, content, enclosing_type, scopes);
                collect_references(child, ctx, enclosing_type, scopes, refs, depth + 1);
                scopes.pop();
                continue;
            }
            "let_declaration" => {
                // Evaluate RHS/type under existing scope BEFORE binding/shadowing LHS.
                collect_references(child, ctx, enclosing_type, scopes, refs, depth + 1);
                if let Some(var) = child
                    .child_by_field_name("pattern")
                    .and_then(|p| extract_ident_pattern(p, content))
                {
                    let ty = child
                        .child_by_field_name("type")
                        .and_then(|t| unwrap_receiver_type(t, content, enclosing_type))
                        .or_else(|| {
                            child
                                .child_by_field_name("value")
                                .and_then(|v| infer_expr_type(v, content, enclosing_type))
                        });
                    if let Some(ty_name) = ty {
                        scopes.insert(var.to_string(), ty_name);
                    } else {
                        scopes.shadow(var.to_string());
                    }
                }
                continue;
            }
            "call_expression" => {
                extract_rust_call(child, ctx, enclosing_type, scopes, refs);
            }
            "type_identifier" => {
                refs.push_mention(node_text(child, content));
            }
            "identifier" => {
                if let Some((name, is_member)) = macro_token_call(child, content) {
                    refs.push_call(name, is_member);
                } else {
                    let ident = node_text(child, content).trim();
                    if is_const_ident(ident) {
                        refs.push_mention(ident);
                    }
                }
            }
            _ => {}
        }
        collect_references(child, ctx, enclosing_type, scopes, refs, depth + 1);
    }
}

fn is_const_ident(s: &str) -> bool {
    s.len() >= 2
        && s.bytes().any(|b| b.is_ascii_uppercase())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn extract_rust_call(
    call_node: Node,
    ctx: &RustWalkCtx<'_>,
    enclosing_type: Option<&str>,
    scopes: &ScopeStack,
    refs: &mut RefCollector,
) {
    let content = ctx.content;
    let Some(mut func_node) = call_node.child_by_field_name("function") else {
        return;
    };
    while func_node.kind() == "generic_function" {
        let Some(inner) = func_node.child_by_field_name("function") else {
            break;
        };
        func_node = inner;
    }
    match func_node.kind() {
        "identifier" => {
            let name = node_text(func_node, content).trim();
            if !matches!(name, "Some" | "Ok" | "Err") {
                refs.push_call(name, false);
            }
        }
        "field_expression" => {
            let Some(field_node) = func_node.child_by_field_name("field") else {
                return;
            };
            let func_name = node_text(field_node, content).trim();
            if func_name.is_empty() {
                return;
            }
            if let Some(val_node) = func_node.child_by_field_name("value") {
                let recv = node_text(val_node, content).trim();
                if recv == "self" {
                    if let Some(ty) = enclosing_type {
                        refs.push_call(&format!("{ty}::{func_name}"), false);
                        return;
                    }
                } else if let Some(ty) = scopes.get(recv) {
                    refs.push_call(&format!("{ty}::{func_name}"), false);
                    return;
                }
            }
            refs.push_call(func_name, true);
        }
        "scoped_identifier" => {
            let Some(name_node) = func_node.child_by_field_name("name") else {
                return;
            };
            let func_name = node_text(name_node, content).trim();
            if func_name.is_empty() {
                return;
            }
            if let Some(path_node) = func_node.child_by_field_name("path") {
                if let Some(ty) = extract_scoped_type(path_node, content, enclosing_type) {
                    if func_name == "new" || func_name == "default" {
                        refs.push_call(ty, false);
                    }
                    refs.push_call(&format!("{ty}::{func_name}"), false);
                    return;
                }
                let (root_seg, last_seg) = extract_path_ends(path_node, content);
                if is_rust_primitive_type(root_seg) {
                    return;
                }
                if ctx.external_set.contains(root_seg) {
                    refs.push_call(&format!("{root_seg}::{func_name}"), false);
                    return;
                }
                if ctx.external_set.contains(last_seg) {
                    refs.push_call(&format!("{last_seg}::{func_name}"), false);
                    return;
                }
                if ctx.outline_mods.contains(last_seg) {
                    refs.push_call(&format!("{last_seg}:{func_name}"), false);
                    return;
                }
            }
            refs.push_call(func_name, false);
        }
        _ => {}
    }
}

fn extract_path_ends<'a>(path_node: Node, content: &'a str) -> (&'a str, &'a str) {
    let mut root_node = path_node;
    while matches!(
        root_node.kind(),
        "scoped_identifier" | "scoped_type_identifier" | "generic_type"
    ) {
        let next = root_node
            .child_by_field_name("path")
            .or_else(|| root_node.child_by_field_name("type"));
        let Some(n) = next else { break };
        root_node = n;
    }
    let root_seg = node_text(root_node, content).trim();
    let last_seg = match path_node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => path_node
            .child_by_field_name("name")
            .map(|n| node_text(n, content).trim())
            .unwrap_or(root_seg),
        _ => node_text(path_node, content).trim(),
    };
    (root_seg, last_seg)
}

fn extract_scoped_type<'a>(
    mut path_node: Node,
    content: &'a str,
    enclosing_type: Option<&'a str>,
) -> Option<&'a str> {
    let (root_seg, _) = extract_path_ends(path_node, content);

    for _ in 0..8 {
        match path_node.kind() {
            "generic_type" | "qualified_type" => {
                path_node = path_node.child_by_field_name("type")?;
            }
            "bracketed_type" => {
                path_node = path_node.named_child(0)?;
            }
            _ => break,
        }
    }
    let seg_node = match path_node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => path_node.child_by_field_name("name")?,
        _ => path_node,
    };
    let seg = node_text(seg_node, content).trim();
    if seg.is_empty() {
        return None;
    }
    if seg == "Self" {
        return Some(enclosing_type.unwrap_or("Self"));
    }
    if seg.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Some(seg);
    }
    if matches!(root_seg, "std" | "core" | "alloc") {
        return Some(seg);
    }
    None
}

fn is_rust_primitive_type(s: &str) -> bool {
    matches!(
        s,
        "bool"
            | "char"
            | "str"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
    )
}

fn macro_token_call<'a>(node: Node, content: &'a str) -> Option<(&'a str, bool)> {
    if node.kind() != "identifier" || node.parent()?.kind() != "token_tree" {
        return None;
    }
    let next = node.next_sibling()?;
    if next.kind() != "token_tree" || !node_text(next, content).starts_with('(') {
        return None;
    }
    let is_member = node.prev_sibling().is_some_and(|p| p.kind() == ".");
    Some((node_text(node, content), is_member))
}

fn is_local_use_root(first_seg: &str, aliases: &AliasSet, local_mods: &HashSet<String>) -> bool {
    matches!(first_seg, "crate" | "self" | "super")
        || aliases.matches_rust(first_seg)
        || local_mods.contains(first_seg)
}

fn use_arg_root_segment<'a>(arg: Node, content: &'a str) -> &'a str {
    let mut cur = arg;
    for _ in 0..32 {
        match cur.kind() {
            "scoped_identifier" | "scoped_use_list" | "use_as_clause" | "use_wildcard" => {
                if let Some(p) = cur.child_by_field_name("path") {
                    cur = p;
                    continue;
                }
                if let Some(first) = cur.named_child(0) {
                    cur = first;
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    node_text(cur, content)
        .trim()
        .split("::")
        .next()
        .unwrap_or("")
        .trim()
}

fn collect_local_use_aliases(
    node: Node,
    content: &str,
    out: &mut HashMap<String, (String, String)>,
    aliases: &AliasSet,
    local_mods: &HashSet<String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            let first = use_arg_root_segment(arg, content);
            if is_local_use_root(first, aliases, local_mods) {
                let mut prefix = Vec::new();
                walk_use_ast_aliases(arg, content, aliases, &mut prefix, out, depth_truncated, 0);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_local_use_aliases(
            c,
            content,
            out,
            aliases,
            local_mods,
            depth_truncated,
            depth + 1,
        );
    }
}

fn record_use_binding(
    prefix: &[String],
    alias: Option<&str>,
    aliases: &AliasSet,
    out: &mut HashMap<String, (String, String)>,
) {
    let mut segs: Vec<&str> = prefix
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty() && !matches!(*s, "crate" | "self" | "super"))
        .collect();
    if segs
        .first()
        .is_some_and(|first| aliases.matches_rust(first))
    {
        segs.remove(0);
    }
    let Some(orig) = segs.pop() else {
        return;
    };
    if orig.is_empty() || orig == "self" || orig == "_" {
        return;
    }
    let hint = segs.join("/");
    match alias {
        Some(a) if !a.is_empty() && a != "_" && a != orig => {
            out.insert(a.to_string(), (hint, orig.to_string()));
        }
        None if !hint.is_empty() => {
            out.entry(orig.to_string())
                .or_insert_with(|| (hint, orig.to_string()));
        }
        _ => {}
    }
}

fn walk_use_ast_aliases(
    node: Node,
    content: &str,
    aliases: &AliasSet,
    prefix: &mut Vec<String>,
    out: &mut HashMap<String, (String, String)>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "scoped_use_list" => {
            let base_len = prefix.len();
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, content, prefix, depth_truncated, depth + 1);
            }
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for c in list.named_children(&mut cursor) {
                    walk_use_ast_aliases(
                        c,
                        content,
                        aliases,
                        prefix,
                        out,
                        depth_truncated,
                        depth + 1,
                    );
                }
            }
            prefix.truncate(base_len);
        }
        "use_list" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                walk_use_ast_aliases(c, content, aliases, prefix, out, depth_truncated, depth + 1);
            }
        }
        "use_as_clause" => {
            let base_len = prefix.len();
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, content, prefix, depth_truncated, depth + 1);
            }
            let alias = node
                .child_by_field_name("alias")
                .map(|a| node_text(a, content).trim());
            record_use_binding(prefix, alias, aliases, out);
            prefix.truncate(base_len);
        }
        "scoped_identifier" | "identifier" => {
            let base_len = prefix.len();
            push_path_segments(node, content, prefix, depth_truncated, depth + 1);
            record_use_binding(prefix, None, aliases, out);
            prefix.truncate(base_len);
        }
        _ => {}
    }
}

fn push_path_segments(
    node: Node,
    content: &str,
    prefix: &mut Vec<String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "scoped_identifier" => {
            if let Some(p) = node.child_by_field_name("path") {
                push_path_segments(p, content, prefix, depth_truncated, depth + 1);
            }
            if let Some(n) = node.child_by_field_name("name") {
                let seg = node_text(n, content).trim();
                if !seg.is_empty() {
                    prefix.push(seg.to_string());
                }
            }
        }
        "identifier" | "crate" | "self" | "super" => {
            let seg = node_text(node, content).trim();
            if !seg.is_empty() {
                prefix.push(seg.to_string());
            }
        }
        _ => {}
    }
}

fn collect_file_external_imports(
    root: Node,
    content: &str,
    aliases: &AliasSet,
    local_mods: &HashSet<String>,
    depth_truncated: &Cell<bool>,
) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_uses_external(
        root,
        content,
        &mut out,
        aliases,
        local_mods,
        depth_truncated,
        0,
    );
    out.items
}

fn collect_uses_external(
    node: Node,
    content: &str,
    out: &mut ImportSet,
    aliases: &AliasSet,
    local_mods: &HashSet<String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            let first = use_arg_root_segment(arg, content);
            if !first.is_empty() && !is_local_use_root(first, aliases, local_mods) {
                let mut parent_seg: Option<String> = None;
                walk_use_ast_externals(
                    arg,
                    content,
                    first,
                    &mut parent_seg,
                    out,
                    depth_truncated,
                    0,
                );
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_uses_external(
            c,
            content,
            out,
            aliases,
            local_mods,
            depth_truncated,
            depth + 1,
        );
    }
}

fn walk_use_ast_externals(
    node: Node,
    content: &str,
    root_crate: &str,
    parent_seg: &mut Option<String>,
    out: &mut ImportSet,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "scoped_use_list" => {
            let prev = parent_seg.clone();
            if let Some(path) = node.child_by_field_name("path") {
                let (_, last) = extract_path_ends(path, content);
                if !last.is_empty() {
                    *parent_seg = Some(last.to_string());
                }
            }
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for c in list.named_children(&mut cursor) {
                    walk_use_ast_externals(
                        c,
                        content,
                        root_crate,
                        parent_seg,
                        out,
                        depth_truncated,
                        depth + 1,
                    );
                }
            }
            *parent_seg = prev;
        }
        "use_list" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                walk_use_ast_externals(
                    c,
                    content,
                    root_crate,
                    parent_seg,
                    out,
                    depth_truncated,
                    depth + 1,
                );
            }
        }
        "use_as_clause" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                let a = node_text(alias, content).trim();
                if a != "_" && a != "self" {
                    out.push(a);
                }
            }
        }
        "scoped_identifier" => {
            if let Some(name) = node.child_by_field_name("name") {
                let n = node_text(name, content).trim();
                if n != "self" {
                    out.push(n);
                }
            }
        }
        "identifier" => {
            let n = node_text(node, content).trim();
            if n != "self" {
                out.push(n);
            }
        }
        "self" => {
            // `use std::io::{self, Read}` binds `io`, never `"self"`.
            if let Some(p) = parent_seg.as_deref() {
                out.push(p);
            }
        }
        "use_wildcard" => {
            // Record the root crate (e.g. `predicates` from `use predicates::prelude::*;`)
            // so `predicates::str::contains` classifies as external.
            out.push(root_crate);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn refs_of(src: &str, symbol: &str) -> Vec<String> {
        parse_rust("t.rs", src, &AliasSet::default())
            .unwrap()
            .into_iter()
            .find(|s| s.name == symbol)
            .unwrap_or_else(|| panic!("no symbol {symbol}"))
            .calls
    }

    #[test]
    fn bare_and_scoped_calls_are_extracted() {
        let src = r#"
            fn helper() {}
            fn caller() {
                helper();
                Foo::bar();
                let _ = format!("{}", other());
            }
        "#;
        let calls = refs_of(src, "caller");
        assert!(calls.contains(&"helper".to_string()));
        assert!(calls.contains(&"Foo::bar".to_string()));
        assert!(calls.contains(&"other".to_string()));
    }

    #[test]
    fn const_signatures_are_not_cut_at_a_brace_in_a_string() {
        let src = "const FMT: &str = \"{hello}\";";
        let syms = parse_rust("t.rs", src, &AliasSet::default()).unwrap();
        assert_eq!(syms[0].signature, src);
    }

    #[test]
    fn deeply_nested_use_trees_do_not_overflow_stack() {
        let src = format!(
            "use crate::{}{}{};",
            "{".repeat(2000),
            "a as b",
            "}".repeat(2000)
        );
        let (_, diag) = parse_rust_with_diagnostics("b.rs", &src, &AliasSet::default()).unwrap();
        assert!(diag.depth_truncated);
    }

    #[test]
    fn nested_external_use_trees_parse_cleanly_without_self_or_braces() {
        let src = r#"
            use std::io::{self, Read};
            use serde::{de::{Deserialize as De, Visitor}, Serialize};
            pub fn f() {}
        "#;
        let syms = parse_rust("t.rs", src, &AliasSet::default()).unwrap();
        let exts = &syms[0].external_imports;
        assert!(exts.contains(&"io".to_string()), "{exts:?}");
        assert!(exts.contains(&"Read".to_string()), "{exts:?}");
        assert!(exts.contains(&"De".to_string()), "{exts:?}");
        assert!(exts.contains(&"Visitor".to_string()), "{exts:?}");
        assert!(exts.contains(&"Serialize".to_string()), "{exts:?}");
        assert!(!exts.contains(&"self".to_string()), "{exts:?}");
        assert!(exts.iter().all(|e| !e.contains('}')), "{exts:?}");
    }

    #[test]
    fn cfg_test_and_local_receiver_types_are_captured() {
        let src = r#"
            pub fn run() {
                let db = Database::open();
                db.load_all_symbols();
                const LOCAL_ONLY: usize = 5;
            }
            #[cfg(test)]
            mod tests {
                #[test]
                fn checks_it() {}
            }
        "#;
        let syms = parse_rust("src/x.rs", src, &AliasSet::default()).unwrap();
        let run = syms.iter().find(|s| s.name == "run").unwrap();
        assert!(run.is_public());
        assert!(!run.is_test_flag());
        assert!(
            run.calls
                .contains(&"Database::load_all_symbols".to_string()),
            "{:?}",
            run.calls
        );
        // Typed receiver must not double-emit bare member call.
        assert!(
            !run.member_calls.contains(&"load_all_symbols".to_string()),
            "{:?}",
            run.member_calls
        );
        assert!(
            !syms.iter().any(|s| s.name == "LOCAL_ONLY"),
            "function-local const leaked: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        let test_fn = syms.iter().find(|s| s.name == "checks_it").unwrap();
        assert!(test_fn.is_test_flag());
    }

    #[test]
    fn linear_comment_run_is_fast() {
        let mut src = "// c\n".repeat(1000);
        src.push_str("pub fn f() {}\n");
        let start = Instant::now();
        let syms = parse_rust("c.rs", &src, &AliasSet::default()).unwrap();
        let dt = start.elapsed();
        assert_eq!(syms.len(), 1);
        assert!(dt.as_millis() < 100, "took {:?}", dt);
    }

    #[test]
    fn serde_skip_serializing_if_and_raw_string_injection_defense() {
        let src = r##"
            struct S {
                #[serde(default, skip_serializing_if = "is_zero_u8")]
                flags: u8,
                #[serde(rename = r#"default = "evil""#)]
                name: String,
            }
            fn is_zero_u8(v: &u8) -> bool { *v == 0 }
            fn evil() {}
        "##;
        let syms = parse_rust("s.rs", src, &AliasSet::default()).unwrap();
        let is_zero = syms.iter().find(|s| s.name == "is_zero_u8").unwrap();
        let evil = syms.iter().find(|s| s.name == "evil").unwrap();
        assert!(is_zero.is_attr_entry());
        assert!(!evil.is_attr_entry());
    }

    #[test]
    fn rust_flags_reject_cfg_not_test_and_pub_crate_and_detect_attr_entries() {
        let src = r#"
            #![cfg_attr(feature = "latest", allow(dead_code))]
            #[cfg(not(test))]
            pub(crate) fn f() {}
            #[cfg(feature = "latest")]
            pub fn g() {}
            #[tokio::main]
            async fn main() {}
            #[proc_macro_derive(MyDerive)]
            pub fn my_derive() {}
        "#;
        let syms = parse_rust("a.rs", src, &AliasSet::default()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert_eq!(f.flags, 0, "flags={:#b}", f.flags);
        let g = syms.iter().find(|s| s.name == "g").unwrap();
        assert!(g.is_public());
        assert!(!g.is_test_flag());
        let main = syms.iter().find(|s| s.name == "main").unwrap();
        assert!(main.is_attr_entry());
        let md = syms.iter().find(|s| s.name == "my_derive").unwrap();
        assert!(md.is_attr_entry());
        assert!(md.is_public());
    }

    #[test]
    fn inner_cfg_test_marks_file_and_module_items() {
        let src = "#![cfg(test)]\nfn helper() {}\n";
        let syms = parse_rust("src/t.rs", src, &AliasSet::default()).unwrap();
        assert!(syms[0].is_test_flag());
    }

    #[test]
    fn sibling_mod_use_is_local_not_external() {
        let src = r#"
            pub mod lang;
            pub use lang::Language;
            pub fn parse_file() { Language::from_path(); }
        "#;
        let syms = parse_rust("src/parser/mod.rs", src, &AliasSet::default()).unwrap();
        let pf = syms.iter().find(|s| s.name == "parse_file").unwrap();
        assert!(!pf.external_imports.contains(&"Language".to_string()));
        assert!(pf.calls.contains(&"Language::from_path".to_string()));
    }

    #[test]
    fn lexical_scope_prevents_shadowing_and_match_scrutinee_phantoms() {
        let src = r#"
            struct Foo;
            struct Bar;
            impl Bar { fn new() -> Self { Self } fn to_other() -> i32 { 0 } }
            fn f(x: Foo) {
                x.a();
                let x = Bar::new();
                x.b();
                let y = Bar::to_other();
                y.c();
                let z = match Order::parse() { _ => Report::new() };
                z.render();
            }
        "#;
        let calls = refs_of(src, "f");
        assert!(calls.contains(&"Foo::a".to_string()), "{calls:?}");
        assert!(!calls.contains(&"Bar::a".to_string()), "{calls:?}");
        assert!(calls.contains(&"Bar::b".to_string()), "{calls:?}");
        assert!(!calls.contains(&"Bar::c".to_string()), "{calls:?}");
        assert!(!calls.contains(&"Order::render".to_string()), "{calls:?}");
    }

    #[test]
    fn non_aliased_local_use_emits_module_hint_and_primitive_not_struct() {
        let src = r#"
            use crate::a::dup;
            use predicates::prelude::*;
            pub fn c() {
                dup();
                let _ = predicates::str::contains("x");
                let _ = u32::from_be_bytes([0; 4]);
            }
        "#;
        let syms = parse_rust("lib.rs", src, &AliasSet::default()).unwrap();
        let c = syms.iter().find(|s| s.name == "c").unwrap();
        assert!(c.calls.contains(&"a:dup".to_string()), "{:?}", c.calls);
        assert!(
            !c.calls.contains(&"str::contains".to_string()),
            "{:?}",
            c.calls
        );
        assert!(
            !c.calls.contains(&"u32::from_be_bytes".to_string()),
            "{:?}",
            c.calls
        );
    }
}
