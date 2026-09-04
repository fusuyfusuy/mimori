use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use tree_sitter::{Node, Parser};

pub fn parse_python(file: &str, content: &str) -> Result<Vec<Symbol>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE.into();
    parser.set_language(&language)?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_python_node(root, content, file, None, &mut symbols);

    Ok(symbols)
}

fn walk_python_node(
    node: Node,
    content: &str,
    file: &str,
    parent_class: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let kind = node.kind();
    match kind {
        "function_definition" | "async_function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let full_name = match parent_class {
                    Some(pc) => format!("{}::{}", pc, name),
                    None => name.to_string(),
                };

                let sym_kind = if parent_class.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };

                let symbol = create_symbol(node, content, file, full_name, sym_kind);
                symbols.push(symbol);
            }
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Class);
                symbols.push(symbol);

                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_python_node(child, content, file, Some(name), symbols);
                    }
                }
                return;
            }
        }
        "decorated_definition" => {
            if let Some(def_node) = node.child_by_field_name("definition") {
                walk_python_node(def_node, content, file, parent_class, symbols);
                return;
            }
        }
        "assignment" if parent_class.is_none() => {
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "identifier" {
                    let name = node_text(left, content);
                    if name.chars().any(|c| c.is_alphabetic())
                        && name.chars().all(|c| !c.is_alphabetic() || c.is_uppercase())
                    {
                        let symbol = create_symbol(node, content, file, name.to_string(), SymbolKind::Constant);
                        symbols.push(symbol);
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_python_node(child, content, file, parent_class, symbols);
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
        doc: None,
        centrality: 0.0,
        references,
    }
}

fn collect_references(node: Node, content: &str, refs: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call" {
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
    if let Some(idx) = body.find(':') {
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
