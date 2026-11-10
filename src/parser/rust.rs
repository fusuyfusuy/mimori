use crate::model::{Symbol, SymbolKind};
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

pub fn parse_rust(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let tree = RUST_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_rust_node(root, content, file, None, &mut symbols, 0);
    let mut import_aliases = HashMap::new();
    collect_local_use_aliases(root, content, &mut import_aliases, aliases, 0);
    super::apply_import_aliases(&mut symbols, &import_aliases);
    let external_imports: Arc<[String]> =
        collect_file_external_imports(root, content, aliases).into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

const MAX_AST_DEPTH: usize = 512;

fn walk_rust_node(
    node: Node,
    content: &str,
    file: &str,
    parent_type: Option<&str>,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    match kind {
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };

                let sym_kind = if parent_type.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };

                let symbol = create_symbol(node, content, file, full_name, sym_kind, parent_type);
                symbols.push(symbol);
            }
        }
        "struct_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Struct,
                    Some(name),
                );
                symbols.push(symbol);
            }
        }
        "enum_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Enum,
                    Some(name),
                );
                symbols.push(symbol);
            }
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Trait,
                    Some(name),
                );
                symbols.push(symbol);

                // Recurse into trait items
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_rust_node(child, content, file, Some(name), symbols, depth + 1);
                }
                return;
            }
        }
        "impl_item" => {
            let type_name = extract_impl_type_name(node, content);

            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_rust_node(child, content, file, type_name, symbols, depth + 1);
                }
                return;
            }
        }
        "type_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::TypeAlias,
                    parent_type,
                );
                symbols.push(symbol);
            }
        }
        "const_item" | "static_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let full_name = match parent_type {
                    Some(pt) => format!("{}::{}", pt, name),
                    None => name.to_string(),
                };
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    full_name,
                    SymbolKind::Constant,
                    parent_type,
                );
                symbols.push(symbol);
            }
        }
        _ => {}
    }

    let next_parent = if kind == "function_item" {
        None
    } else {
        parent_type
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust_node(child, content, file, next_parent, symbols, depth + 1);
    }
}

fn extract_impl_type_name<'a>(impl_node: Node, content: &'a str) -> Option<&'a str> {
    let mut cur = impl_node.child_by_field_name("type")?;
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

#[derive(Default)]
struct RefCollector {
    calls: Vec<String>,
    mentions: Vec<String>,
    mention_set: HashSet<String>,
    counts: HashMap<String, u32>,
    member_calls: Vec<String>,
    bare_calls: HashSet<String>,
}

impl RefCollector {
    fn push_call(&mut self, name: &str, is_member: bool) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(c) = self.counts.get_mut(name) {
            *c += 1;
        } else {
            let owned = name.to_string();
            self.counts.insert(owned.clone(), 1);
            self.calls.push(owned);
        }
        if is_member {
            if !self.bare_calls.contains(name) && !self.member_calls.iter().any(|c| c == name) {
                self.member_calls.push(name.to_string());
            }
        } else if self.bare_calls.insert(name.to_string()) {
            self.member_calls.retain(|c| c != name);
        }
    }

    fn push_mention(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.mention_set.contains(name) {
            let owned = name.to_string();
            self.mention_set.insert(owned.clone());
            self.mentions.push(owned);
        }
    }
}

fn create_symbol(
    node: Node,
    content: &str,
    file: &str,
    name: String,
    kind: SymbolKind,
    enclosing_type: Option<&str>,
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let signature = extract_signature(node, content, raw_body, &kind);

    let mut refs = RefCollector::default();
    collect_references(node, content, enclosing_type, &mut refs, 0);

    Symbol {
        name,
        kind,
        file: file.to_string(),
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
    }
}

fn collect_references(
    node: Node,
    content: &str,
    enclosing_type: Option<&str>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
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
            extract_rust_call(child, content, enclosing_type, refs);
        } else if kind == "type_identifier" {
            refs.push_mention(node_text(child, content));
        } else if let Some((name, is_member)) = macro_token_call(child, content) {
            refs.push_call(name, is_member);
        }
        collect_references(child, content, enclosing_type, refs, depth + 1);
    }
}

fn extract_rust_call(
    call_node: Node,
    content: &str,
    enclosing_type: Option<&str>,
    refs: &mut RefCollector,
) {
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
            if let (Some(val_node), Some(ty)) =
                (func_node.child_by_field_name("value"), enclosing_type)
            {
                if node_text(val_node, content).trim() == "self" && !func_name.is_empty() {
                    refs.push_call(&format!("{ty}::{func_name}"), false);
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
            let ty = func_node
                .child_by_field_name("path")
                .and_then(|p| extract_scoped_type(p, content, enclosing_type));
            if let Some(ty) = ty {
                if func_name == "new" || func_name == "default" {
                    refs.push_call(ty, false);
                }
                refs.push_call(&format!("{ty}::{func_name}"), false);
            } else {
                refs.push_call(func_name, false);
            }
        }
        _ => {}
    }
}

fn extract_scoped_type<'a>(
    mut path_node: Node,
    content: &'a str,
    enclosing_type: Option<&'a str>,
) -> Option<&'a str> {
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

/// Macro arguments (`format!("{}", self.f())`) parse as flat token trees, not
/// `call_expression`s. An identifier directly followed by a `(...)` token tree
/// is a call; a preceding `.` marks it a member call.
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

fn extract_signature(node: Node, content: &str, body: &str, kind: &SymbolKind) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();

    // A const or static has no body to split off.
    if matches!(kind, SymbolKind::Constant | SymbolKind::Variable) {
        return first_line.to_string();
    }

    // Prefer AST body node boundary over string searching so braces in doc comments/generics don't corrupt the signature.
    if let Some(body_node) = node.child_by_field_name("body") {
        let sig_raw = &content[node.start_byte()..body_node.start_byte()];
        let sig = sig_raw.trim().replace('\n', " ");
        if !sig.is_empty() {
            return sig;
        }
    }

    if let Some(idx) = body.find('{') {
        let sig = body[..idx].trim().replace('\n', " ");
        if !sig.is_empty() {
            return sig;
        }
    }
    first_line.to_string()
}

fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    &content[node.start_byte()..node.end_byte()]
}

#[derive(Default)]
struct ImportSet {
    items: Vec<String>,
    seen: HashSet<String>,
}

impl ImportSet {
    fn push(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && name != "*" && !self.seen.contains(name) {
            let owned = name.to_string();
            self.seen.insert(owned.clone());
            self.items.push(owned);
        }
    }
}

/// Local names bound by `use` to anything outside the current crate's
/// namespace (`crate::`, `self::`, `super::` stay local). `std::` and third-
/// party crates can never be workspace symbols, so calls matching these
/// names are external, never resolved locally.
fn collect_file_external_imports(root: Node, content: &str, aliases: &AliasSet) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_uses(root, content, &mut out, aliases, 0);
    out.items
}

fn collect_local_use_aliases(
    node: Node,
    content: &str,
    out: &mut std::collections::HashMap<String, (String, String)>,
    aliases: &AliasSet,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "use_declaration" {
        let mut text = node_text(node, content).trim();
        if let Some(rest) = text.strip_prefix("pub") {
            text = rest.trim();
            if let Some(rest_paren) = text.strip_prefix('(') {
                if let Some((_, after)) = rest_paren.split_once(')') {
                    text = after.trim();
                }
            }
        }
        let tree = text
            .strip_prefix("use")
            .unwrap_or(text)
            .trim()
            .trim_end_matches(';')
            .trim();
        let tree_first = tree.split("::").next().unwrap_or(tree).trim();
        if tree_first == "crate"
            || tree_first == "self"
            || tree_first == "super"
            || aliases.matches(tree_first)
        {
            collect_use_tree_aliases(tree, "", out);
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_local_use_aliases(c, content, out, aliases, depth + 1);
    }
}

fn collect_use_tree_aliases(
    tree: &str,
    prefix: &str,
    out: &mut std::collections::HashMap<String, (String, String)>,
) {
    let tree = tree.trim();
    if let Some((head, rest)) = tree.split_once('{') {
        let head = head.trim().trim_end_matches("::");
        let next_prefix = if prefix.is_empty() {
            head.to_string()
        } else if head.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix}::{head}")
        };
        let rest = rest.trim_end_matches('}');
        for part in split_top_level_comma(rest) {
            collect_use_tree_aliases(part.trim(), &next_prefix, out);
        }
        return;
    }
    if let Some((orig_path, alias)) = tree.split_once(" as ") {
        let alias = alias.trim();
        let full_path = if prefix.is_empty() {
            orig_path.trim().to_string()
        } else {
            format!("{prefix}::{}", orig_path.trim())
        };
        let mut segs: Vec<&str> = full_path
            .split("::")
            .filter(|s| !s.is_empty() && !matches!(*s, "crate" | "self" | "super"))
            .collect();
        if let Some(orig) = segs.pop() {
            let orig = orig.trim();
            if !orig.is_empty() && !alias.is_empty() && alias != "_" && orig != alias {
                let hint = segs.join("/");
                out.insert(alias.to_string(), (hint, orig.to_string()));
            }
        }
    }
}

fn collect_uses(node: Node, content: &str, out: &mut ImportSet, aliases: &AliasSet, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "use_declaration" {
        let mut text = node_text(node, content).trim();
        if let Some(rest) = text.strip_prefix("pub") {
            text = rest.trim();
            if let Some(rest_paren) = text.strip_prefix('(') {
                if let Some((_, after)) = rest_paren.split_once(')') {
                    text = after.trim();
                }
            }
        }
        let tree = text
            .strip_prefix("use")
            .unwrap_or(text)
            .trim()
            .trim_end_matches(';')
            .trim();
        let tree_first = tree.split("::").next().unwrap_or(tree).trim();
        if !(tree_first == "crate"
            || tree_first == "self"
            || tree_first == "super"
            || aliases.matches(tree_first))
        {
            collect_use_tree_names(tree, out);
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_uses(c, content, out, aliases, depth + 1);
    }
}

fn split_top_level_comma(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        parts.push(&s[start..]);
    }
    parts
}

/// Last-segment binding of each imported path; `{a, b as c}` enumerates.
/// Glob (`*`) binds nothing enumerable — its names stay resolvable, a
/// documented recall-over-precision choice for globs.
fn collect_use_tree_names(tree: &str, out: &mut ImportSet) {
    let tree = tree.trim();
    if let Some((_head, rest)) = tree.split_once('{') {
        let rest = rest.trim_end_matches('}');
        for part in split_top_level_comma(rest) {
            let part = part.trim();
            if part.is_empty() || part == "*" {
                continue;
            }
            if let Some((_, alias)) = part.split_once(" as ") {
                out.push(alias.trim());
            } else if part.contains('{') {
                collect_use_tree_names(part, out);
            } else {
                let last = part.rsplit("::").next().unwrap_or(part).trim();
                out.push(last);
            }
        }
        return;
    }
    if tree == "*" {
        return;
    }
    if let Some((_path, alias)) = tree.split_once(" as ") {
        out.push(alias.trim());
    } else {
        let last = tree.rsplit("::").next().unwrap_or(tree).trim();
        out.push(last);
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
    fn method_calls_are_recorded_as_references() {
        // Regression M4: `self.save(1)` recorded the literal "self.save",
        // which matched no symbol, so method calls produced no edges at all.
        let src = "struct S; impl S { fn save(&self) {} fn run(&self) { self.save(); } }";
        assert!(
            refs_of(src, "S::run").contains(&"save".to_string()),
            "got {:?}",
            refs_of(src, "S::run")
        );
    }

    #[test]
    fn const_signatures_are_not_cut_at_a_brace_in_a_string() {
        // Regression S4: extract_signature ignored `kind` in this parser only,
        // truncating at the first '{' wherever it appeared.
        let src = r#"pub const BRACE: &str = "{ not a body }";"#;
        let sym = parse_rust("t.rs", src, &AliasSet::default())
            .unwrap()
            .into_iter()
            .find(|s| s.name == "BRACE")
            .unwrap();
        assert!(
            sym.signature.contains("not a body"),
            "got: {}",
            sym.signature
        );
    }

    #[test]
    fn function_signatures_still_stop_at_the_body() {
        let src = "pub fn real(x: u64) -> u64 { x }";
        let sym = parse_rust("t.rs", src, &AliasSet::default())
            .unwrap()
            .into_iter()
            .find(|s| s.name == "real")
            .unwrap();
        assert_eq!(sym.signature, "pub fn real(x: u64) -> u64");
    }

    #[test]
    fn path_calls_still_resolve_to_the_final_segment() {
        let src = "fn f() { crate::db::query_user(); }";
        assert!(refs_of(src, "f").contains(&"query_user".to_string()));
    }

    #[test]
    fn chained_method_calls_record_the_called_method() {
        let src = "fn f() { let x = builder().with_name().finish(); }";
        let refs = refs_of(src, "f");
        assert!(refs.contains(&"finish".to_string()), "got {refs:?}");
    }

    #[test]
    fn nested_use_trees_extract_clean_identifiers() {
        let src =
            "use std::{collections::{HashMap, HashSet}, io};\nfn f() { let _ = HashMap::new(); }";
        let syms = parse_rust("t.rs", src, &AliasSet::default()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert!(
            f.external_imports.contains(&"HashMap".to_string()),
            "got {:?}",
            f.external_imports
        );
        assert!(
            f.external_imports.contains(&"HashSet".to_string()),
            "got {:?}",
            f.external_imports
        );
        assert!(
            f.external_imports.contains(&"io".to_string()),
            "got {:?}",
            f.external_imports
        );
        assert!(
            !f.external_imports.iter().any(|s| s.contains('}')),
            "trailing brace found: {:?}",
            f.external_imports
        );
    }

    #[test]
    fn pub_use_crate_is_not_external_import() {
        let src = "pub use crate::db::query_user;\npub(crate) use crate::model::User;\nuse anyhow::Result;\nfn f() {}";
        let syms = parse_rust("t.rs", src, &AliasSet::default()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert!(
            !f.external_imports.contains(&"query_user".to_string()),
            "crate export misflagged as external: {:?}",
            f.external_imports
        );
        assert!(
            !f.external_imports.contains(&"User".to_string()),
            "crate export misflagged as external: {:?}",
            f.external_imports
        );
        assert!(
            f.external_imports.contains(&"Result".to_string()),
            "external anyhow::Result missing: {:?}",
            f.external_imports
        );
    }
}
