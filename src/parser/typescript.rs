use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use tree_sitter::{Node, Parser};

pub fn parse_typescript(file: &str, content: &str, is_tsx: bool) -> Result<Vec<Symbol>> {
    let mut parser = Parser::new();
    let language = if is_tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    parser.set_language(&language)?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse TypeScript source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_ts_node(root, content, file, None, &mut symbols);

    Ok(symbols)
}

fn walk_ts_node(
    node: Node,
    content: &str,
    file: &str,
    parent_class: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Function);
                symbols.push(symbol);
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Class);
                symbols.push(symbol);

                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_ts_node(child, content, file, Some(name), symbols);
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Interface);
                symbols.push(symbol);
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::TypeAlias);
                symbols.push(symbol);
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Enum);
                symbols.push(symbol);
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let full_name = match parent_class {
                    Some(pc) => format!("{}::{}", pc, name),
                    None => name.to_string(),
                };
                let symbol = create_symbol(node, content, file, full_name, SymbolKind::Method);
                symbols.push(symbol);
            }
        }
        "variable_declarator" => {
            if let (Some(name_node), Some(value_node)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ) {
                let name = node_text(name_node, content);
                let val_kind = value_node.kind();
                if val_kind == "arrow_function" || val_kind == "function" {
                    let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Function);
                    symbols.push(symbol);
                } else if val_kind == "call_expression" {
                    let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Variable);
                    symbols.push(symbol);

                    if let Some(args_node) = value_node.child_by_field_name("arguments") {
                        let mut arg_cursor = args_node.walk();
                        for arg in args_node.children(&mut arg_cursor) {
                            if arg.kind() == "object" {
                                extract_object_literal_members(arg, content, file, name, symbols);
                            }
                        }
                        // Return as the class_declaration arm does. Falling
                        // through let the generic recursion revisit the same
                        // method_definition nodes with parent_class = None,
                        // emitting every shorthand method a second time.
                        return;
                    }
                } else if is_top_level_const(node, content) {
                    let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Constant);
                    symbols.push(symbol);
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ts_node(child, content, file, parent_class, symbols);
    }
}

fn create_symbol(
    node: Node,
    content: &str,
    file: &str,
    name: String,
    kind: SymbolKind,
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let body = node_text(node, content).to_string();
    let signature = extract_signature(&body, &kind);

    let mut references = Vec::new();
    collect_references(node, content, &mut references);

    Symbol {
        name,
        kind,
        file: file.to_string(),
        start_line: start_pos.row + 1,
        end_line: end_pos.row + 1,
        signature,
        body,
        centrality: 0.0,
        references,
    }
}

fn collect_references(node: Node, content: &str, refs: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some(func_node) = child.child_by_field_name("function") {
                let text = node_text(func_node, content);
                let func_name = text.rsplit('.').next().unwrap_or(text).trim();
                if !func_name.is_empty() && !refs.contains(&func_name.to_string()) {
                    refs.push(func_name.to_string());
                }
            }
        } else if child.kind() == "type_identifier" {
            let tname = node_text(child, content).trim();
            if !tname.is_empty() && !refs.contains(&tname.to_string()) {
                refs.push(tname.to_string());
            }
        }
        collect_references(child, content, refs);
    }
}

fn extract_object_literal_members(
    object_node: Node,
    content: &str,
    file: &str,
    parent_name: &str,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = object_node.walk();
    for child in object_node.children(&mut cursor) {
        if child.kind() == "pair" {
            if let Some(key_node) = child.child_by_field_name("key") {
                let key_name = node_text(key_node, content).trim_matches('\'').trim_matches('"');
                if !key_name.is_empty() {
                    let full_name = format!("{}::{}", parent_name, key_name);
                    let symbol = create_symbol(child, content, file, full_name, SymbolKind::Method);
                    symbols.push(symbol);
                }
            }
        } else if child.kind() == "method_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let key_name = node_text(name_node, content).trim_matches('\'').trim_matches('"');
                if !key_name.is_empty() {
                    let full_name = format!("{}::{}", parent_name, key_name);
                    let symbol = create_symbol(child, content, file, full_name, SymbolKind::Method);
                    symbols.push(symbol);
                }
            }
        }
    }
}

fn is_top_level_const(node: Node, content: &str) -> bool {
    if let Some(parent) = node.parent() {
        if parent.kind() == "lexical_declaration" {
            let decl_text = node_text(parent, content).trim_start();
            if decl_text.starts_with("const ") {
                if let Some(grandparent) = parent.parent() {
                    return grandparent.kind() == "export_statement" || grandparent.kind() == "program";
                }
                return true;
            }
        }
    }
    false
}

fn extract_signature(body: &str, kind: &SymbolKind) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();
    match kind {
        SymbolKind::Constant | SymbolKind::Variable => first_line.to_string(),
        _ => {
            if let Some(idx) = body.find('{') {
                let sig = body[..idx].trim().replace('\n', " ");
                if !sig.is_empty() && sig.len() < 120 {
                    return sig;
                }
            }
            first_line.to_string()
        }
    }
}

fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    &content[node.start_byte()..node.end_byte()]
}
