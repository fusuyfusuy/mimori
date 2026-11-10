use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tree_sitter::{Node, Parser};

thread_local! {
    static PY_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("Error loading Python grammar");
        p
    });
}

pub fn parse_python(file: &str, content: &str) -> Result<Vec<Symbol>> {
    let tree = PY_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_python_node(root, content, file, None, &mut symbols, None, 0);
    let mut import_aliases = HashMap::new();
    collect_py_local_aliases(root, content, &mut import_aliases, 0);
    super::apply_import_aliases(&mut symbols, &import_aliases);
    let external_imports: Arc<[String]> = collect_file_external_imports(root, content).into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

const MAX_AST_DEPTH: usize = 512;

fn is_module_level_assignment(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "module"
        || (parent.kind() == "expression_statement"
            && parent.parent().is_some_and(|gp| gp.kind() == "module"))
}

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
                let symbol = create_symbol(
                    symbol_node,
                    content,
                    file,
                    full_name,
                    sym_kind,
                    parent_class,
                );
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
                    Some(name),
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
        "assignment" | "annotated_assignment"
            if parent_class.is_none() && is_module_level_assignment(node) =>
        {
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
                    let symbol =
                        create_symbol(node, content, file, name.to_string(), sym_kind, None);
                    symbols.push(symbol);
                }
            }
        }
        _ => {}
    }

    let next_parent = if matches!(kind, "function_definition" | "async_function_definition") {
        None
    } else {
        parent_class
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_python_node(child, content, file, next_parent, symbols, None, depth + 1);
    }
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
        if name.is_empty() || name == "self" || name == "cls" {
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
        if name.is_empty() || name == "self" || name == "cls" {
            return;
        }
        if !self.mention_set.contains(name) {
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
    enclosing_class: Option<&str>,
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let signature = extract_signature(node, content, raw_body);

    let mut refs = RefCollector::default();
    collect_references(node, content, enclosing_class, &mut refs, 0);

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
    enclosing_class: Option<&str>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if node.kind() != "decorated_definition"
            && matches!(
                kind,
                "function_definition"
                    | "async_function_definition"
                    | "class_definition"
                    | "decorated_definition"
            )
        {
            continue;
        }
        if kind == "call" {
            extract_py_call(child, content, enclosing_class, refs);
        } else if kind == "attribute" {
            if let Some(attr) = child.child_by_field_name("attribute") {
                refs.push_mention(node_text(attr, content));
            }
        } else if kind == "type" {
            collect_type_mentions(child, content, refs, depth + 1);
            continue;
        }
        collect_references(child, content, enclosing_class, refs, depth + 1);
    }
}

fn extract_py_call(
    call_node: Node,
    content: &str,
    enclosing_class: Option<&str>,
    refs: &mut RefCollector,
) {
    if let Some(func_node) = call_node.child_by_field_name("function") {
        // `x.build()` (attribute function) is a member call:
        // file-local only. Bare `build()` may resolve globally.
        if func_node.kind() == "attribute" {
            let func_name = func_node
                .child_by_field_name("attribute")
                .map(|a| node_text(a, content).trim())
                .unwrap_or_else(|| {
                    let t = node_text(func_node, content);
                    t.rsplit('.').next().unwrap_or(t).trim()
                });
            refs.push_call(func_name, true);
            if let (Some(obj), Some(cls_name)) =
                (func_node.child_by_field_name("object"), enclosing_class)
            {
                let recv = node_text(obj, content).trim();
                if (recv == "self" || recv == "cls") && !func_name.is_empty() {
                    refs.push_call(&format!("{cls_name}::{func_name}"), false);
                }
            }
        } else {
            let text = node_text(func_node, content);
            let func_name = text.rsplit('.').next().unwrap_or(text).trim();
            refs.push_call(func_name, false);
        }
    }
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut acursor = args_node.walk();
        for arg in args_node.children(&mut acursor) {
            if arg.kind() == "identifier" {
                refs.push_mention(node_text(arg, content));
            } else if arg.kind() == "attribute" {
                if let Some(attr) = arg.child_by_field_name("attribute") {
                    refs.push_mention(node_text(attr, content));
                }
            } else if arg.kind() == "keyword_argument" {
                if let Some(val) = arg.child_by_field_name("value") {
                    if val.kind() == "identifier" {
                        refs.push_mention(node_text(val, content));
                    } else if val.kind() == "attribute" {
                        if let Some(attr) = val.child_by_field_name("attribute") {
                            refs.push_mention(node_text(attr, content));
                        }
                    }
                }
            }
        }
    }
}

fn collect_type_mentions(node: Node, content: &str, refs: &mut RefCollector, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if kind == "identifier" {
        refs.push_mention(node_text(node, content));
    } else if kind == "attribute" {
        if let Some(attr) = node.child_by_field_name("attribute") {
            refs.push_mention(node_text(attr, content));
        }
    } else if kind == "type" {
        let text = node_text(node, content).trim();
        if !text.is_empty() && !text.contains('[') && !text.contains('(') && !text.contains('|') {
            let simple = text.rsplit('.').next().unwrap_or(text).trim();
            refs.push_mention(simple);
        }
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_type_mentions(c, content, refs, depth + 1);
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

struct PyFromImport<'a> {
    module: &'a str,
    items: Vec<(&'a str, Option<&'a str>)>,
}

fn parse_from_import_ast<'a>(node: Node, content: &'a str) -> Option<PyFromImport<'a>> {
    let module = node
        .child_by_field_name("module_name")
        .map(|m| node_text(m, content).trim())?;
    let mut items = Vec::new();
    let mut seen_import = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import" {
            seen_import = true;
            continue;
        }
        if !seen_import {
            continue;
        }
        match child.kind() {
            "aliased_import" => {
                if let Some(orig_node) = child.child_by_field_name("name") {
                    let orig = node_text(orig_node, content).trim();
                    let alias = child
                        .child_by_field_name("alias")
                        .map(|a| node_text(a, content).trim());
                    if !orig.is_empty() {
                        items.push((orig, alias));
                    }
                }
            }
            "dotted_name" => {
                let orig = node_text(child, content).trim();
                if !orig.is_empty() {
                    items.push((orig, None));
                }
            }
            _ => {}
        }
    }
    Some(PyFromImport { module, items })
}

/// Local names bound to non-relative imports. `from pkg import y as z`
/// binds `z`; `from .local import y` binds nothing (resolvable locally).
fn collect_file_external_imports(root: Node, content: &str) -> Vec<String> {
    let mut out = ImportSet::default();
    collect_py_imports(root, content, &mut out, 0);
    out.items
}

fn collect_py_local_aliases(
    node: Node,
    content: &str,
    out: &mut HashMap<String, (String, String)>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_from_statement" {
        if let Some(parsed) = parse_from_import_ast(node, content) {
            let top_segment = parsed
                .module
                .trim_start_matches('.')
                .split('.')
                .next()
                .unwrap_or("")
                .trim();
            let is_local = parsed.module.starts_with('.') || !PYTHON_STDLIB.contains(&top_segment);
            if is_local {
                let hint = parsed.module.trim_start_matches('.').replace('.', "/");
                for (orig, alias) in parsed.items {
                    if let Some(alias) = alias.filter(|a| !a.is_empty() && *a != orig) {
                        out.insert(alias.to_string(), (hint.clone(), orig.to_string()));
                    }
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_py_local_aliases(c, content, out, depth + 1);
    }
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

fn collect_py_imports(node: Node, content: &str, out: &mut ImportSet, depth: usize) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if kind == "import_statement" {
        // `import a.b as c` binds `c`, else the top segment `a`.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "aliased_import" => {
                    if let Some(orig_node) = child.child_by_field_name("name") {
                        let orig = node_text(orig_node, content).trim();
                        let top = orig.split('.').next().unwrap_or(orig).trim();
                        if PYTHON_STDLIB.contains(&top) {
                            let bound = child
                                .child_by_field_name("alias")
                                .map(|a| node_text(a, content).trim())
                                .unwrap_or(top);
                            out.push(bound);
                        }
                    }
                }
                "dotted_name" => {
                    let orig = node_text(child, content).trim();
                    let top = orig.split('.').next().unwrap_or(orig).trim();
                    if PYTHON_STDLIB.contains(&top) {
                        out.push(top);
                    }
                }
                _ => {}
            }
        }
        return;
    }
    if kind == "import_from_statement" {
        if let Some(parsed) = parse_from_import_ast(node, content) {
            if !parsed.module.starts_with('.') {
                let top_segment = parsed
                    .module
                    .split('.')
                    .next()
                    .unwrap_or(parsed.module)
                    .trim();
                if PYTHON_STDLIB.contains(&top_segment) {
                    for (orig, alias) in parsed.items {
                        if let Some(alias) = alias {
                            out.push(alias);
                        } else {
                            out.push(orig.split('.').next().unwrap_or(orig));
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
