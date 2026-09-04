use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use tree_sitter::{Node, Parser};

pub fn parse_go(file: &str, content: &str) -> Result<Vec<Symbol>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE.into();
    parser.set_language(&language)?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_go_node(root, content, file, &mut symbols);

    Ok(symbols)
}

fn walk_go_node(node: Node, content: &str, file: &str, symbols: &mut Vec<Symbol>) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Function);
                symbols.push(symbol);
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let receiver_type = extract_receiver_type(node, content);
                let full_name = match receiver_type {
                    Some(rt) => format!("{}::{}", rt, name),
                    None => name.to_string(),
                };

                let symbol = create_symbol(node, content, file, full_name, SymbolKind::Method);
                symbols.push(symbol);
            }
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, content);
                        let type_node = child.child_by_field_name("type");
                        let sym_kind = match type_node.map(|t| t.kind()) {
                            Some("struct_type") => SymbolKind::Struct,
                            Some("interface_type") => SymbolKind::Interface,
                            _ => SymbolKind::TypeAlias,
                        };

                        let symbol = create_symbol(node, content, file, name.to_string(), sym_kind);
                        symbols.push(symbol);
                    }
                }
            }
        }
        "const_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "const_spec" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, content);
                        let symbol = create_symbol(child, content, file, name.to_string(), SymbolKind::Constant);
                        symbols.push(symbol);
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_go_node(child, content, file, symbols);
    }
}

fn extract_receiver_type<'a>(node: Node<'a>, content: &'a str) -> Option<&'a str> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                let mut t = node_text(type_node, content);
                if t.starts_with('*') {
                    t = &t[1..];
                }
                return Some(t);
            }
        }
    }
    None
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
    let signature = extract_signature(&body);

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
        }
        collect_references(child, content, refs);
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

fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    &content[node.start_byte()..node.end_byte()]
}
