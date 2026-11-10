use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tree_sitter::{Node, Parser};

thread_local! {
    static GO_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Error loading Go grammar");
        p
    });
}

pub fn parse_go(file: &str, content: &str) -> Result<Vec<Symbol>> {
    let tree = GO_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    let imported_packages = collect_file_imported_packages(root, content);
    walk_go_node(root, content, file, &mut symbols, &imported_packages, 0);
    let external_imports: Arc<[String]> = collect_file_external_imports(root, content).into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

const MAX_AST_DEPTH: usize = 512;

#[derive(Default)]
struct ImportSet {
    items: Vec<String>,
    seen: HashSet<String>,
}

impl ImportSet {
    fn push(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.seen.contains(name) {
            let owned = name.to_string();
            self.seen.insert(owned.clone());
            self.items.push(owned);
        }
    }
}

fn collect_file_imported_packages(root: Node, content: &str) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_go_all_imports(root, content, &mut out, 0);
    out.items
}

fn collect_go_all_imports(node: Node, content: &str, out: &mut ImportSet, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_spec" {
        let (name, path) = parse_go_import_spec(node, content);
        if let Some(p) = path {
            match name {
                Some("_") | Some(".") => {}
                None => out.push(p.rsplit('/').next().unwrap_or(p)),
                Some(n) => out.push(n),
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_go_all_imports(c, content, out, depth + 1);
    }
}

fn parse_go_import_spec<'a>(node: Node, content: &'a str) -> (Option<&'a str>, Option<&'a str>) {
    let mut name: Option<&str> = None;
    let mut path: Option<&str> = None;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "package_identifier" | "identifier" | "blank_identifier" | "dot" => {
                name = Some(node_text(c, content).trim());
            }
            "interpreted_string_literal" | "raw_string_literal" => {
                path = Some(node_text(c, content).trim_matches(['"', '`', ' ']));
            }
            _ => {}
        }
    }
    (name, path)
}

fn walk_go_node(
    node: Node,
    content: &str,
    file: &str,
    symbols: &mut Vec<Symbol>,
    imported_packages: &[String],
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                    imported_packages,
                );
                symbols.push(symbol);
            }
            return;
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let (recv_ident, receiver_type) = extract_receiver(node, content);
                let full_name = match receiver_type {
                    Some(rt) => format!("{}::{}", rt, name),
                    None => name.to_string(),
                };

                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    full_name,
                    SymbolKind::Method,
                    recv_ident.zip(receiver_type),
                    imported_packages,
                );
                symbols.push(symbol);
            }
            return;
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            let mut specs = Vec::new();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    specs.push(child);
                }
            }
            let is_grouped = specs.len() > 1;
            for child in specs {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, content);
                    let type_node = child.child_by_field_name("type");
                    let sym_kind = match type_node.map(|t| t.kind()) {
                        Some("struct_type") => SymbolKind::Struct,
                        Some("interface_type") => SymbolKind::Interface,
                        _ => SymbolKind::TypeAlias,
                    };

                    let sym_node = if is_grouped { child } else { node };
                    let symbol = create_symbol(
                        sym_node,
                        content,
                        file,
                        name.to_string(),
                        sym_kind,
                        None,
                        imported_packages,
                    );
                    symbols.push(symbol);
                }
            }
            return;
        }
        "const_declaration" => {
            extract_spec_symbols(
                node,
                content,
                file,
                "const_spec",
                SymbolKind::Constant,
                imported_packages,
                symbols,
            );
            return;
        }
        "var_declaration" => {
            extract_spec_symbols(
                node,
                content,
                file,
                "var_spec",
                SymbolKind::Variable,
                imported_packages,
                symbols,
            );
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_go_node(child, content, file, symbols, imported_packages, depth + 1);
    }
}

fn extract_spec_symbols(
    decl_node: Node,
    content: &str,
    file: &str,
    spec_kind: &str,
    sym_kind: SymbolKind,
    imported_packages: &[String],
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() == spec_kind {
            emit_named_specs(child, content, file, &sym_kind, imported_packages, symbols);
        } else if child.kind().ends_with("_spec_list") {
            let mut lcursor = child.walk();
            for sub in child.children(&mut lcursor) {
                if sub.kind() == spec_kind {
                    emit_named_specs(sub, content, file, &sym_kind, imported_packages, symbols);
                }
            }
        }
    }
}

fn emit_named_specs(
    spec_node: Node,
    content: &str,
    file: &str,
    sym_kind: &SymbolKind,
    imported_packages: &[String],
    symbols: &mut Vec<Symbol>,
) {
    let mut ncursor = spec_node.walk();
    for name_node in spec_node.children_by_field_name("name", &mut ncursor) {
        let name = node_text(name_node, content).trim();
        if !name.is_empty() && name != "_" {
            let symbol = create_symbol(
                spec_node,
                content,
                file,
                name.to_string(),
                sym_kind.clone(),
                None,
                imported_packages,
            );
            symbols.push(symbol);
        }
    }
}

fn extract_receiver<'a>(node: Node, content: &'a str) -> (Option<&'a str>, Option<&'a str>) {
    let Some(receiver) = node.child_by_field_name("receiver") else {
        return (None, None);
    };
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            let ident = child
                .child_by_field_name("name")
                .map(|n| node_text(n, content).trim())
                .filter(|s| !s.is_empty() && *s != "_");
            let recv_ty = child.child_by_field_name("type").and_then(|type_node| {
                let mut t = node_text(type_node, content).trim();
                if let Some(rest) = t.strip_prefix('*') {
                    t = rest.trim();
                }
                if let Some((base, _)) = t.split_once('[') {
                    t = base.trim();
                }
                (!t.is_empty()).then_some(t)
            });
            return (ident, recv_ty);
        }
    }
    (None, None)
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
    receiver: Option<(&str, &str)>,
    imported_packages: &[String],
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let signature = extract_signature(raw_body);

    let mut refs = RefCollector::default();
    collect_references(node, content, receiver, imported_packages, &mut refs, 0);

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
    receiver: Option<(&str, &str)>,
    imported_packages: &[String],
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "call_expression" {
            extract_go_call(child, content, receiver, imported_packages, refs);
        } else if kind == "selector_expression" {
            if let Some(field) = child.child_by_field_name("field") {
                refs.push_mention(node_text(field, content));
            }
        } else if kind == "type_identifier" {
            refs.push_mention(node_text(child, content));
        }

        collect_references(child, content, receiver, imported_packages, refs, depth + 1);
    }
}

fn extract_go_call(
    call_node: Node,
    content: &str,
    receiver: Option<(&str, &str)>,
    imported_packages: &[String],
    refs: &mut RefCollector,
) {
    if let Some(func_node) = call_node.child_by_field_name("function") {
        // `x.F()` (selector) is a member call: file-local only.
        let mut is_member = func_node.kind() == "selector_expression";
        let func_name = if is_member {
            func_node
                .child_by_field_name("field")
                .map(|f| node_text(f, content).trim())
                .unwrap_or_else(|| {
                    let t = node_text(func_node, content);
                    t.rsplit('.').next().unwrap_or(t).trim()
                })
        } else {
            let t = node_text(func_node, content);
            t.rsplit('.').next().unwrap_or(t).trim()
        };

        if is_member {
            if let Some(operand) = func_node.child_by_field_name("operand") {
                let op_text = node_text(operand, content).trim();
                if imported_packages.iter().any(|p| p == op_text) {
                    is_member = false;
                } else if let Some((_, recv_type)) =
                    receiver.filter(|(id, _)| *id == op_text && !func_name.is_empty())
                {
                    refs.push_call(&format!("{recv_type}::{func_name}"), false);
                }
            }
        }

        refs.push_call(func_name, is_member);
    }
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut acursor = args_node.walk();
        for arg in args_node.children(&mut acursor) {
            if arg.kind() == "identifier" {
                refs.push_mention(node_text(arg, content));
            }
        }
    }
}

fn extract_signature(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();
    if let Some(idx) = body.find('{') {
        let sig = body[..idx].trim().replace('\n', " ");
        if !sig.is_empty() {
            return sig;
        }
    }
    first_line.to_string()
}

/// Local names bound to stdlib (dotless-path) imports. `import "fmt"`
/// binds `fmt`; dotted paths (module + third-party) stay resolvable —
/// the local package may itself be indexed.
fn collect_file_external_imports(root: Node, content: &str) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_go_imports(root, content, &mut out, 0);
    out.items
}

fn collect_go_imports(node: Node, content: &str, out: &mut ImportSet, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_spec" {
        let (name, path) = parse_go_import_spec(node, content);
        if let Some(p) = path.filter(|p| !p.contains('.')) {
            match name {
                Some("_") | Some(".") => {}
                None => out.push(p.rsplit('/').next().unwrap_or(p)),
                Some(n) => out.push(n),
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_go_imports(c, content, out, depth + 1);
    }
}

fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    &content[node.start_byte()..node.end_byte()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_mentions_collected() {
        let code = r#"
package main

func ProcessUser(u User, item *Item) Result {
    val := u.Name
    helper(val, item)
    return Result{}
}
"#;
        let symbols = parse_go("main.go", code).unwrap();
        let sym = symbols.iter().find(|s| s.name == "ProcessUser").unwrap();

        // 1. type_identifier nodes: User, Item, Result
        assert!(
            sym.mentions.contains(&"User".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"Item".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"Result".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // 2. selector_expression field access: Name
        assert!(
            sym.mentions.contains(&"Name".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // 3. Call arguments that are identifiers: val, item
        assert!(
            sym.mentions.contains(&"val".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"item".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // Ensure calls is intact and not polluted by mentions
        assert!(sym.calls.contains(&"helper".to_string()));
        assert!(!sym.calls.contains(&"val".to_string()));
    }
}
