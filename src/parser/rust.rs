use crate::model::{symbol_flags, Symbol, SymbolKind};
use crate::parser::common::{
    extract_signature_ast, is_valid_ident, node_text, ImportSet, RefCollector, MAX_AST_DEPTH,
};
use crate::workspace::AliasSet;
use anyhow::Result;
use std::cell::RefCell;
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
}

pub fn parse_rust(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let tree = RUST_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;

    let root = tree.root_node();
    let external_list = collect_file_external_imports(root, content, aliases);
    let external_set: HashSet<String> = external_list.iter().cloned().collect();
    let serde_targets = collect_serde_hook_targets(root, content);

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
    };

    let mut symbols = Vec::new();
    walk_rust_node(
        root,
        &ctx,
        None,
        false,
        false,
        false,
        false,
        &mut symbols,
        0,
    );

    let mut import_aliases = HashMap::new();
    collect_local_use_aliases(root, content, &mut import_aliases, aliases, 0);
    super::apply_import_aliases(&mut symbols, &import_aliases);

    let external_imports: Arc<[String]> = external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

#[allow(clippy::too_many_arguments)]
fn walk_rust_node(
    node: Node,
    ctx: &RustWalkCtx<'_>,
    parent_type: Option<&str>,
    in_test: bool,
    in_trait_impl: bool,
    in_fn: bool,
    in_serde_mod: bool,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    let content = ctx.content;
    let attrs = collect_preceding_attrs(node, content);
    let node_is_test = in_test || ctx.file_is_test || attrs_mark_test(&attrs);
    let node_is_attr_entry = in_serde_mod || attrs_mark_entry(&attrs);

    match kind {
        "mod_item" => {
            let mod_name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, content).trim())
                .unwrap_or("");
            let next_serde_mod = in_serde_mod || ctx.serde_targets.contains(mod_name);
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_rust_node(
                    child,
                    ctx,
                    None,
                    node_is_test,
                    false,
                    false,
                    next_serde_mod,
                    symbols,
                    depth + 1,
                );
            }
            return;
        }
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };
                let sym_kind = if parent_type.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let flags = compute_flags(
                    node,
                    name,
                    ctx,
                    node_is_test,
                    in_trait_impl,
                    node_is_attr_entry,
                );
                let symbol =
                    create_symbol(node, ctx, full_name, sym_kind, parent_type, flags, None);
                symbols.push(symbol);
            }
            // Walk nested items inside functions (e.g. nested helper fns) with in_fn = true
            // so function-local `const` items are skipped.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_rust_node(
                    child,
                    ctx,
                    None,
                    node_is_test,
                    false,
                    true,
                    in_serde_mod,
                    symbols,
                    depth + 1,
                );
            }
            return;
        }
        "function_signature_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };
                let flags = compute_flags(node, name, ctx, node_is_test, true, node_is_attr_entry);
                let symbol = create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::Method,
                    parent_type,
                    flags,
                    None,
                );
                symbols.push(symbol);
            }
            return;
        }
        "struct_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = compute_flags(node, name, ctx, node_is_test, false, node_is_attr_entry);
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Struct,
                    Some(name),
                    flags,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "enum_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = compute_flags(node, name, ctx, node_is_test, false, node_is_attr_entry);
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Enum,
                    Some(name),
                    flags,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = compute_flags(node, name, ctx, node_is_test, false, node_is_attr_entry);
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Trait,
                    Some(name),
                    flags,
                    None,
                );
                symbols.push(symbol);

                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_rust_node(
                        child,
                        ctx,
                        Some(name),
                        node_is_test,
                        true,
                        false,
                        in_serde_mod,
                        symbols,
                        depth + 1,
                    );
                }
                return;
            }
        }
        "impl_item" => {
            let type_name = extract_impl_type_name(node, content);
            let trait_name = extract_impl_trait_name(node, content);
            let is_trait_impl = trait_name.is_some();

            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if let Some(tr) = trait_name {
                        // Record trait mention on methods of `impl Trait for Type`.
                        if child.kind() == "function_item" {
                            if let Some(name_node) = child.child_by_field_name("name") {
                                let name = node_text(name_node, content).trim();
                                let full_name = match type_name {
                                    Some(pt) => format!("{}::{}", pt, name),
                                    None => name.to_string(),
                                };
                                let child_attrs = collect_preceding_attrs(child, content);
                                let child_test = node_is_test || attrs_mark_test(&child_attrs);
                                let flags = compute_flags(
                                    child,
                                    name,
                                    ctx,
                                    child_test,
                                    true,
                                    node_is_attr_entry || attrs_mark_entry(&child_attrs),
                                );
                                let symbol = create_symbol(
                                    child,
                                    ctx,
                                    full_name,
                                    SymbolKind::Method,
                                    type_name,
                                    flags,
                                    Some(tr),
                                );
                                symbols.push(symbol);
                                continue;
                            }
                        }
                    }
                    walk_rust_node(
                        child,
                        ctx,
                        type_name,
                        node_is_test,
                        is_trait_impl,
                        false,
                        in_serde_mod,
                        symbols,
                        depth + 1,
                    );
                }
                return;
            }
        }
        "type_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };
                let flags = compute_flags(
                    node,
                    name,
                    ctx,
                    node_is_test,
                    in_trait_impl,
                    node_is_attr_entry,
                );
                let symbol = create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::TypeAlias,
                    parent_type,
                    flags,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "const_item" | "static_item" if !in_fn => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };
                let flags = compute_flags(
                    node,
                    name,
                    ctx,
                    node_is_test,
                    in_trait_impl,
                    node_is_attr_entry,
                );
                let symbol = create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::Constant,
                    parent_type,
                    flags,
                    None,
                );
                symbols.push(symbol);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust_node(
            child,
            ctx,
            parent_type,
            node_is_test,
            in_trait_impl,
            in_fn,
            in_serde_mod,
            symbols,
            depth + 1,
        );
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
    if has_pub_visibility(node) {
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

fn has_pub_visibility(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return true;
        }
    }
    false
}

fn collect_preceding_attrs<'a>(node: Node, content: &'a str) -> Vec<&'a str> {
    let mut attrs = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => attrs.push(node_text(p, content)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    attrs
}

fn attrs_mark_test(attrs: &[&str]) -> bool {
    attrs.iter().any(|raw| {
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        compact == "#[test]"
            || compact.ends_with("::test]")
            || compact.contains("::test(")
            || compact.starts_with("#[cfg(test)]")
            || (compact.starts_with("#[cfg(") && compact.contains("test"))
    })
}

fn attrs_mark_entry(attrs: &[&str]) -> bool {
    attrs.iter().any(|raw| {
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        compact.contains("no_mangle")
            || compact.contains("export_name")
            || compact.contains("serde(")
    })
}

/// Scan file-level `#[serde(default = "fn", with = "mod", serialize_with = "fn", deserialize_with = "fn")]`
/// so referenced serde callback functions/modules are marked `IS_ATTR_ENTRY`.
fn collect_serde_hook_targets(root: Node, content: &str) -> HashSet<String> {
    let mut targets = HashSet::new();
    walk_serde_attrs(root, content, &mut targets, 0);
    targets
}

fn walk_serde_attrs(node: Node, content: &str, out: &mut HashSet<String>, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "attribute_item" {
        let text = node_text(node, content);
        if text.contains("serde(") {
            for key in ["default", "with", "serialize_with", "deserialize_with"] {
                let mut rest = text;
                while let Some(idx) = rest.find(key) {
                    rest = &rest[idx + key.len()..];
                    let trimmed = rest.trim_start();
                    if let Some(after_eq) = trimmed.strip_prefix('=') {
                        let after_eq = after_eq.trim_start();
                        if let Some(after_q) = after_eq.strip_prefix('"') {
                            if let Some(end_q) = after_q.find('"') {
                                let target = after_q[..end_q].trim();
                                if let Some(last) = target.rsplit("::").next() {
                                    if is_valid_ident(last) {
                                        out.insert(last.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_serde_attrs(c, content, out, depth + 1);
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

/// Infer simple local variable -> type name mappings inside a Rust function
/// (parameters, typed `let` bindings, `Type::ctor(...)` calls, `Type { ... }`)
/// so `var.method()` emits a qualified `Type::method` cross-file call.
fn collect_local_receiver_types(
    fn_node: Node,
    content: &str,
    enclosing_type: Option<&str>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
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
                    map.insert(var.to_string(), ty_name);
                }
            }
        }
    }
    if let Some(body) = fn_node.child_by_field_name("body") {
        collect_let_types(body, content, enclosing_type, &mut map, 0);
    }
    map
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

fn collect_let_types(
    node: Node,
    content: &str,
    enclosing_type: Option<&str>,
    out: &mut HashMap<String, String>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if matches!(
        kind,
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item" | "mod_item"
    ) {
        return;
    }
    if kind == "let_declaration" {
        if let Some(var) = node
            .child_by_field_name("pattern")
            .and_then(|p| extract_ident_pattern(p, content))
        {
            let ty = node
                .child_by_field_name("type")
                .and_then(|t| unwrap_receiver_type(t, content, enclosing_type))
                .or_else(|| {
                    node.child_by_field_name("value")
                        .and_then(|v| infer_expr_type(v, content, enclosing_type))
                });
            if let Some(ty_name) = ty {
                out.insert(var.to_string(), ty_name);
            }
        }
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_let_types(c, content, enclosing_type, out, depth + 1);
    }
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
            "match_expression" => {
                // Handle `let x = match Type::load(...) { Ok(v) => v, ... }`
                expr = expr.child_by_field_name("value")?;
            }
            "call_expression" => {
                let mut fn_node = expr.child_by_field_name("function")?;
                while fn_node.kind() == "generic_function" {
                    fn_node = fn_node.child_by_field_name("function")?;
                }
                if fn_node.kind() == "scoped_identifier" {
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
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let first_line = raw_body.lines().next().unwrap_or("").trim();
    let signature = if matches!(kind, SymbolKind::Constant | SymbolKind::Variable) {
        first_line.to_string()
    } else {
        extract_signature_ast(node, content, first_line)
    };

    let local_types = if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        collect_local_receiver_types(node, content, enclosing_type)
    } else {
        HashMap::new()
    };

    let mut refs = RefCollector::default();
    if let Some(tr) = impl_trait_mention {
        refs.push_mention(tr);
    }
    collect_references(node, ctx, enclosing_type, &local_types, &mut refs, 0);

    Symbol {
        name,
        kind,
        file: ctx.file.to_string(),
        start_line: start_pos.row + 1,
        end_line: end_pos.row + 1,
        signature,
        body: String::new(),
        centrality: 0.0,
        calls: refs.calls,
        mentions: refs.mentions,
        call_counts: refs.counts,
        member_calls: refs.member_calls,
        external_imports: crate::model::symbol::empty_external_imports(),
        flags,
    }
}

fn collect_references(
    node: Node,
    ctx: &RustWalkCtx<'_>,
    enclosing_type: Option<&str>,
    local_types: &HashMap<String, String>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
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
        if kind == "call_expression" {
            extract_rust_call(child, ctx, enclosing_type, local_types, refs);
        } else if kind == "type_identifier" {
            refs.push_mention(node_text(child, content));
        } else if kind == "identifier" {
            if let Some((name, is_member)) = macro_token_call(child, content) {
                refs.push_call(name, is_member);
            } else {
                let ident = node_text(child, content).trim();
                if is_const_ident(ident) {
                    refs.push_mention(ident);
                }
            }
        }
        collect_references(child, ctx, enclosing_type, local_types, refs, depth + 1);
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
    local_types: &HashMap<String, String>,
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
            refs.push_call(node_text(func_node, content), false);
        }
        "field_expression" => {
            let Some(field_node) = func_node.child_by_field_name("field") else {
                return;
            };
            let func_name = node_text(field_node, content).trim();
            refs.push_call(func_name, true);
            if let Some(val_node) = func_node.child_by_field_name("value") {
                let recv = node_text(val_node, content).trim();
                if !func_name.is_empty() {
                    if recv == "self" {
                        if let Some(ty) = enclosing_type {
                            refs.push_call(&format!("{ty}::{func_name}"), false);
                        }
                    } else if let Some(ty) = local_types.get(recv) {
                        refs.push_call(&format!("{ty}::{func_name}"), false);
                    }
                }
            }
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
                // Preserve external module qualifier (e.g. `fs::write`, `serde_json::from_str`)
                // so it hits `external_imports` instead of collapsing to bare `"write"`.
                let (root_seg, last_seg) = extract_path_ends(path_node, content);
                if ctx.external_set.contains(root_seg) || ctx.external_set.contains(last_seg) {
                    refs.push_call(&format!("{last_seg}::{func_name}"), false);
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
    if seg.starts_with(|c: char| c.is_ascii_uppercase()) || is_rust_primitive_type(seg) {
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

fn is_local_use_root(first_seg: &str, aliases: &AliasSet) -> bool {
    matches!(first_seg, "crate" | "self" | "super") || aliases.matches_rust(first_seg)
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
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            let first = use_arg_root_segment(arg, content);
            if is_local_use_root(first, aliases) {
                let mut prefix = Vec::new();
                walk_use_ast_aliases(arg, content, &mut prefix, out, 0);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_local_use_aliases(c, content, out, aliases, depth + 1);
    }
}

fn walk_use_ast_aliases(
    node: Node,
    content: &str,
    prefix: &mut Vec<String>,
    out: &mut HashMap<String, (String, String)>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    match node.kind() {
        "scoped_use_list" => {
            let base_len = prefix.len();
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, content, prefix, depth + 1);
            }
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for c in list.named_children(&mut cursor) {
                    walk_use_ast_aliases(c, content, prefix, out, depth + 1);
                }
            }
            prefix.truncate(base_len);
        }
        "use_list" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                walk_use_ast_aliases(c, content, prefix, out, depth + 1);
            }
        }
        "use_as_clause" => {
            let base_len = prefix.len();
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, content, prefix, depth + 1);
            }
            let alias = node
                .child_by_field_name("alias")
                .map(|a| node_text(a, content).trim())
                .unwrap_or("");
            let mut segs: Vec<&str> = prefix
                .iter()
                .map(String::as_str)
                .filter(|s| !s.is_empty() && !matches!(*s, "crate" | "self" | "super"))
                .collect();
            if let Some(orig) = segs.pop() {
                if !orig.is_empty() && !alias.is_empty() && alias != "_" && orig != alias {
                    let hint = segs.join("/");
                    out.insert(alias.to_string(), (hint, orig.to_string()));
                }
            }
            prefix.truncate(base_len);
        }
        _ => {}
    }
}

fn push_path_segments(node: Node, content: &str, prefix: &mut Vec<String>, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    match node.kind() {
        "scoped_identifier" => {
            if let Some(p) = node.child_by_field_name("path") {
                push_path_segments(p, content, prefix, depth + 1);
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

fn collect_file_external_imports(root: Node, content: &str, aliases: &AliasSet) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_uses_external(root, content, &mut out, aliases, 0);
    out.items
}

fn collect_uses_external(
    node: Node,
    content: &str,
    out: &mut ImportSet,
    aliases: &AliasSet,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            let first = use_arg_root_segment(arg, content);
            if !first.is_empty() && !is_local_use_root(first, aliases) {
                let mut parent_seg: Option<String> = None;
                walk_use_ast_externals(arg, content, &mut parent_seg, out, 0);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_uses_external(c, content, out, aliases, depth + 1);
    }
}

fn walk_use_ast_externals(
    node: Node,
    content: &str,
    parent_seg: &mut Option<String>,
    out: &mut ImportSet,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
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
                    walk_use_ast_externals(c, content, parent_seg, out, depth + 1);
                }
            }
            *parent_seg = prev;
        }
        "use_list" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                walk_use_ast_externals(c, content, parent_seg, out, depth + 1);
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
        "use_wildcard" => {}
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = parse_rust("b.rs", &src, &AliasSet::default());
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
        assert!(
            !syms.iter().any(|s| s.name == "LOCAL_ONLY"),
            "function-local const leaked: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        let test_fn = syms.iter().find(|s| s.name == "checks_it").unwrap();
        assert!(test_fn.is_test_flag());
    }
}
