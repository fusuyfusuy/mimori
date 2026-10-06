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
    walk_python_node(root, content, file, None, &mut symbols, None, 0);
    let external_imports = collect_file_external_imports(root, content);
    for s in &mut symbols {
        s.external_imports = external_imports.clone();
    }

    Ok(symbols)
}

const MAX_AST_DEPTH: usize = 512;

fn walk_python_node(
    node: Node,
    content: &str,
    file: &str,
    parent_class: Option<&str>,
    symbols: &mut Vec<Symbol>,
    outer_node: Option<Node>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
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

                let symbol_node = outer_node.unwrap_or(node);
                let symbol = create_symbol(symbol_node, content, file, full_name, sym_kind);
                symbols.push(symbol);
            }
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol_node = outer_node.unwrap_or(node);
                let symbol = create_symbol(
                    symbol_node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Class,
                );
                symbols.push(symbol);

                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_python_node(
                            child,
                            content,
                            file,
                            Some(name),
                            symbols,
                            None,
                            depth + 1,
                        );
                    }
                }
                return;
            }
        }
        "decorated_definition" => {
            if let Some(def_node) = node.child_by_field_name("definition") {
                walk_python_node(
                    def_node,
                    content,
                    file,
                    parent_class,
                    symbols,
                    Some(node),
                    depth + 1,
                );
                return;
            }
        }
        "assignment" | "annotated_assignment" if parent_class.is_none() => {
            let left_node = node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("target"));
            if let Some(left) = left_node {
                if left.kind() == "identifier" {
                    let name = node_text(left, content);
                    let is_upper = name.chars().any(|c| c.is_alphabetic())
                        && name.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
                    let sym_kind = if is_upper {
                        SymbolKind::Constant
                    } else {
                        let right = node
                            .child_by_field_name("right")
                            .or_else(|| node.child_by_field_name("value"));
                        if matches!(right.map(|r| r.kind()), Some("call") | Some("await")) {
                            SymbolKind::Variable
                        } else {
                            SymbolKind::Constant
                        }
                    };
                    let symbol = create_symbol(node, content, file, name.to_string(), sym_kind);
                    symbols.push(symbol);
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_python_node(child, content, file, parent_class, symbols, None, depth + 1);
    }
}

fn create_symbol(node: Node, content: &str, file: &str, name: String, kind: SymbolKind) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let body = node_text(node, content).to_string();
    let signature = extract_signature(node, content, &body);

    let mut calls = Vec::new();
    let mut mentions = Vec::new();
    let mut call_counts = std::collections::HashMap::new();
    let mut member_calls = Vec::new();
    collect_references(
        node,
        content,
        &mut calls,
        &mut mentions,
        &mut call_counts,
        &mut member_calls,
        0,
    );

    Symbol {
        name,
        kind,
        file: file.to_string(),
        start_line: start_pos.row + 1,
        end_line: end_pos.row + 1,
        signature,
        body,
        centrality: 0.0,
        calls,
        mentions,
        call_counts,
        member_calls,
        external_imports: Vec::new(),
    }
}

fn collect_references(
    node: Node,
    content: &str,
    calls: &mut Vec<String>,
    mentions: &mut Vec<String>,
    counts: &mut std::collections::HashMap<String, u32>,
    member_calls: &mut Vec<String>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "call" {
            if let Some(func_node) = child.child_by_field_name("function") {
                // `x.build()` (attribute function) is a member call:
                // file-local only. Bare `build()` may resolve globally.
                let is_member = func_node.kind() == "attribute";
                let text = node_text(func_node, content);
                let func_name = text.rsplit('.').next().unwrap_or(text).trim();
                if !func_name.is_empty() {
                    *counts.entry(func_name.to_string()).or_insert(0) += 1;
                    if !calls.iter().any(|c| c == func_name) {
                        calls.push(func_name.to_string());
                    }
                    if is_member && !member_calls.iter().any(|c| c == func_name) {
                        member_calls.push(func_name.to_string());
                    }
                }
            }
            if let Some(args_node) = child.child_by_field_name("arguments") {
                let mut acursor = args_node.walk();
                for arg in args_node.children(&mut acursor) {
                    if arg.kind() == "identifier" {
                        push_mention(mentions, node_text(arg, content));
                    } else if arg.kind() == "attribute" {
                        if let Some(attr) = arg.child_by_field_name("attribute") {
                            push_mention(mentions, node_text(attr, content));
                        }
                    } else if arg.kind() == "keyword_argument" {
                        if let Some(val) = arg.child_by_field_name("value") {
                            if val.kind() == "identifier" {
                                push_mention(mentions, node_text(val, content));
                            } else if val.kind() == "attribute" {
                                if let Some(attr) = val.child_by_field_name("attribute") {
                                    push_mention(mentions, node_text(attr, content));
                                }
                            }
                        }
                    }
                }
            }
        } else if kind == "attribute" {
            if let Some(attr) = child.child_by_field_name("attribute") {
                push_mention(mentions, node_text(attr, content));
            }
        } else if kind == "type" {
            collect_type_mentions(child, content, mentions);
        }
        collect_references(
            child,
            content,
            calls,
            mentions,
            counts,
            member_calls,
            depth + 1,
        );
    }
}

fn collect_type_mentions(node: Node, content: &str, mentions: &mut Vec<String>) {
    let kind = node.kind();
    if kind == "identifier" {
        push_mention(mentions, node_text(node, content));
    } else if kind == "attribute" {
        if let Some(attr) = node.child_by_field_name("attribute") {
            push_mention(mentions, node_text(attr, content));
        }
    } else if kind == "type" {
        let text = node_text(node, content).trim();
        if !text.is_empty() && !text.contains('[') && !text.contains('(') && !text.contains('|') {
            let simple = text.rsplit('.').next().unwrap_or(text).trim();
            push_mention(mentions, simple);
        }
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_type_mentions(c, content, mentions);
    }
}

fn push_mention(mentions: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty() || name == "self" || name == "cls" {
        return;
    }
    if !mentions.iter().any(|m| m == name) {
        mentions.push(name.to_string());
    }
}

fn extract_signature(node: Node, content: &str, body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();

    // If node is a decorated_definition, unpack the inner definition.
    let def_node = if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    };

    if let Some(body_node) = def_node.child_by_field_name("body") {
        let sig_raw = &content[def_node.start_byte()..body_node.start_byte()];
        let sig = sig_raw
            .trim()
            .trim_end_matches(':')
            .trim()
            .replace('\n', " ");
        if !sig.is_empty() {
            return sig;
        }
    }

    first_line.to_string()
}

/// Local names bound to non-relative imports. `from pkg import y as z`
/// binds `z`; `from .local import y` binds nothing (resolvable locally).
fn collect_file_external_imports(root: Node, content: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_py_imports(root, content, &mut out, 0);
    out
}

const PYTHON_STDLIB: &[&str] = &[
    "os",
    "sys",
    "math",
    "json",
    "re",
    "typing",
    "collections",
    "itertools",
    "functools",
    "pathlib",
    "logging",
    "asyncio",
    "datetime",
    "subprocess",
    "unittest",
    "time",
    "random",
    "hashlib",
    "io",
    "urllib",
    "http",
    "abc",
    "copy",
    "tempfile",
    "shutil",
    "glob",
    "pickle",
    "sqlite3",
    "threading",
    "multiprocessing",
    "socket",
    "dataclasses",
    "enum",
    "struct",
    "inspect",
    "traceback",
    "warnings",
];

fn collect_py_imports(node: Node, content: &str, out: &mut Vec<String>, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if kind == "import_statement" {
        // `import a.b as c` binds `c`, else the top segment `a`.
        let text = node_text(node, content);
        let clause = text.strip_prefix("import").unwrap_or(text).trim();
        for part in clause.split(',') {
            let part = part.trim();
            if let Some((_, alias)) = part.split_once(" as ") {
                let top = part.split('.').next().unwrap_or(part).trim();
                if PYTHON_STDLIB.contains(&top) {
                    push_import(out, alias.trim());
                }
            } else {
                let top = part.split('.').next().unwrap_or(part).trim();
                if PYTHON_STDLIB.contains(&top) {
                    push_import(out, top);
                }
            }
        }
        return;
    }
    if kind == "import_from_statement" {
        let text = node_text(node, content);
        let after_from = text.strip_prefix("from").unwrap_or(text).trim();
        if let Some((module, names)) = after_from.split_once(" import ") {
            if !module.trim().starts_with('.') {
                let top_segment = module.split('.').next().unwrap_or(module).trim();
                if PYTHON_STDLIB.contains(&top_segment) {
                    for part in names.split(',') {
                        let part = part.trim().trim_matches(['(', ')', ' ']);
                        if part.is_empty() || part == "*" {
                            continue;
                        }
                        if let Some((_, alias)) = part.split_once(" as ") {
                            push_import(out, alias.trim());
                        } else {
                            push_import(out, part.split('.').next().unwrap_or(part));
                        }
                    }
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_py_imports(c, content, out, depth + 1);
    }
}

fn push_import(out: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !out.contains(&name.to_string()) {
        out.push(name.to_string());
    }
}

fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    &content[node.start_byte()..node.end_byte()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_mentions_collected() {
        let code = r#"
def handle_request(user: User, config: AppConfig) -> Response:
    settings = user.profile
    validate(user, setting_val=config.timeout)
"#;
        let symbols = parse_python("test.py", code).unwrap();
        let sym = symbols.iter().find(|s| s.name == "handle_request").unwrap();

        // 1. Type annotations: User, AppConfig, Response
        assert!(
            sym.mentions.contains(&"User".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"AppConfig".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"Response".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // 2. Call arguments: user, timeout
        assert!(
            sym.mentions.contains(&"user".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // 3. Attributes: profile, timeout
        assert!(
            sym.mentions.contains(&"profile".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );
        assert!(
            sym.mentions.contains(&"timeout".to_string()),
            "Mentions: {:?}",
            sym.mentions
        );

        // Ensure calls is intact and not polluted by mentions
        assert!(sym.calls.contains(&"validate".to_string()));
        assert!(!sym.calls.contains(&"user".to_string()));
    }
}
