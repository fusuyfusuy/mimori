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
    static GO_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Error loading Go grammar");
        p
    });
}

struct GoImports {
    /// Local package identifier -> module hint for first-party imports.
    internal: HashMap<String, String>,
    /// Local package identifiers bound to stdlib or third-party external packages.
    external: HashSet<String>,
    external_list: Vec<String>,
}

pub fn parse_go(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let tree = GO_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    let imports = collect_go_imports(root, content, aliases);
    let file_is_test = file.ends_with("_test.go");
    walk_go_node(root, content, file, file_is_test, &imports, &mut symbols, 0);

    let external_imports: Arc<[String]> = imports.external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

/// Bind Go package name for an import path, stripping trailing major-version
/// suffixes (`/v2`, `/v3`, ...).
fn go_default_pkg_name(path: &str) -> &str {
    let mut segs = path.rsplit('/');
    let last = segs.next().unwrap_or(path);
    if is_go_major_version(last) {
        segs.next().unwrap_or(last)
    } else {
        last
    }
}

fn is_go_major_version(seg: &str) -> bool {
    let Some(rest) = seg.strip_prefix('v') else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn collect_go_imports(root: Node, content: &str, aliases: &AliasSet) -> GoImports {
    let mut internal = HashMap::new();
    let mut ext_set = ImportSet::default();
    walk_go_imports(root, content, aliases, &mut internal, &mut ext_set, 0);
    let external: HashSet<String> = ext_set.items.iter().cloned().collect();
    GoImports {
        internal,
        external,
        external_list: ext_set.items,
    }
}

fn walk_go_imports(
    node: Node,
    content: &str,
    aliases: &AliasSet,
    internal: &mut HashMap<String, String>,
    external: &mut ImportSet,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_spec" {
        let (name, path) = parse_go_import_spec(node, content);
        if let Some(p) = path {
            let bound = match name {
                Some("_") | Some(".") => return,
                Some(n) => n,
                None => go_default_pkg_name(p),
            };
            let is_internal = if aliases.has_go_modules() {
                aliases.matches_go(p)
            } else {
                p.contains('.')
            };
            if is_internal {
                let hint = go_default_pkg_name(p).to_string();
                internal.insert(bound.to_string(), hint);
            } else {
                external.push(bound);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_go_imports(c, content, aliases, internal, external, depth + 1);
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
    file_is_test: bool,
    imports: &GoImports,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let is_test = file_is_test
                    || name.starts_with("Test")
                    || name.starts_with("Benchmark")
                    || name.starts_with("Fuzz")
                    || name.starts_with("Example");
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                    imports,
                    go_flags(name, is_test),
                );
                symbols.push(symbol);
            }
            return;
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
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
                    imports,
                    go_flags(name, file_is_test),
                );
                symbols.push(symbol);
            }
            return;
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            let mut specs = Vec::new();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "type_spec" | "type_alias") {
                    specs.push(child);
                }
            }
            let is_grouped = specs.len() > 1;
            for child in specs {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, content).trim();
                    let type_node = child.child_by_field_name("type");
                    let sym_kind = if child.kind() == "type_alias" {
                        SymbolKind::TypeAlias
                    } else {
                        match type_node.map(|t| t.kind()) {
                            Some("struct_type") => SymbolKind::Struct,
                            Some("interface_type") => SymbolKind::Interface,
                            _ => SymbolKind::TypeAlias,
                        }
                    };
                    let sym_node = if is_grouped { child } else { node };
                    let symbol = create_symbol(
                        sym_node,
                        content,
                        file,
                        name.to_string(),
                        sym_kind,
                        None,
                        imports,
                        go_flags(name, file_is_test),
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
                file_is_test,
                "const_spec",
                SymbolKind::Constant,
                imports,
                symbols,
            );
            return;
        }
        "var_declaration" => {
            extract_spec_symbols(
                node,
                content,
                file,
                file_is_test,
                "var_spec",
                SymbolKind::Variable,
                imports,
                symbols,
            );
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_go_node(
            child,
            content,
            file,
            file_is_test,
            imports,
            symbols,
            depth + 1,
        );
    }
}

fn go_flags(name: &str, is_test: bool) -> u8 {
    let mut flags = 0u8;
    if is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        flags |= symbol_flags::IS_PUBLIC;
    }
    flags
}

#[allow(clippy::too_many_arguments)]
fn extract_spec_symbols(
    decl_node: Node,
    content: &str,
    file: &str,
    file_is_test: bool,
    spec_kind: &str,
    sym_kind: SymbolKind,
    imports: &GoImports,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() == spec_kind {
            emit_named_specs(
                child,
                content,
                file,
                file_is_test,
                &sym_kind,
                imports,
                symbols,
            );
        } else if child.kind().ends_with("_spec_list") {
            let mut lcursor = child.walk();
            for sub in child.children(&mut lcursor) {
                if sub.kind() == spec_kind {
                    emit_named_specs(
                        sub,
                        content,
                        file,
                        file_is_test,
                        &sym_kind,
                        imports,
                        symbols,
                    );
                }
            }
        }
    }
}

fn emit_named_specs(
    spec_node: Node,
    content: &str,
    file: &str,
    file_is_test: bool,
    sym_kind: &SymbolKind,
    imports: &GoImports,
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
                imports,
                go_flags(name, file_is_test),
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
            let recv_ty = child
                .child_by_field_name("type")
                .and_then(|type_node| strip_go_type_name(node_text(type_node, content)));
            return (ident, recv_ty);
        }
    }
    (None, None)
}

fn strip_go_type_name(raw: &str) -> Option<&str> {
    let mut t = raw.trim();
    while let Some(rest) = t.strip_prefix('*') {
        t = rest.trim();
    }
    if let Some((base, _)) = t.split_once('[') {
        t = base.trim();
    }
    if let Some((_, tail)) = t.rsplit_once('.') {
        t = tail.trim();
    }
    is_valid_ident(t).then_some(t)
}

fn collect_go_local_types(fn_node: Node, content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for p in params.children(&mut cursor) {
            if p.kind() == "parameter_declaration" {
                let ty = p
                    .child_by_field_name("type")
                    .and_then(|t| strip_go_type_name(node_text(t, content)));
                if let Some(ty_name) = ty {
                    if ty_name.starts_with(|c: char| c.is_ascii_uppercase()) {
                        let mut ncursor = p.walk();
                        for name_node in p.children_by_field_name("name", &mut ncursor) {
                            let var = node_text(name_node, content).trim();
                            if is_valid_ident(var) {
                                map.insert(var.to_string(), ty_name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    map
}

#[allow(clippy::too_many_arguments)]
fn create_symbol(
    node: Node,
    content: &str,
    file: &str,
    name: String,
    kind: SymbolKind,
    receiver: Option<(&str, &str)>,
    imports: &GoImports,
    flags: u8,
) -> Symbol {
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
        collect_go_local_types(node, content)
    } else {
        HashMap::new()
    };

    let mut refs = RefCollector::default();
    collect_references(node, content, receiver, &local_types, imports, &mut refs, 0);

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
        flags,
    }
}

fn collect_references(
    node: Node,
    content: &str,
    receiver: Option<(&str, &str)>,
    local_types: &HashMap<String, String>,
    imports: &GoImports,
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
            extract_go_call(child, content, receiver, local_types, imports, refs);
        } else if kind == "type_conversion_expression" {
            // tree-sitter-go parses single-argument generic calls (`GenericFn[int](42)`
            // and `pkg.GenericFn[int](42)`) as `type_conversion_expression` with a
            // `generic_type` or `type_instantiation_expression` target.
            if let Some(ty) = child.child_by_field_name("type") {
                if matches!(ty.kind(), "generic_type" | "type_instantiation_expression") {
                    extract_go_callee(ty, content, receiver, local_types, imports, refs);
                }
            }
        } else if kind == "selector_expression" {
            if let Some(field) = child.child_by_field_name("field") {
                refs.push_mention(node_text(field, content));
            }
        } else if kind == "type_identifier" {
            refs.push_mention(node_text(child, content));
        }

        collect_references(
            child,
            content,
            receiver,
            local_types,
            imports,
            refs,
            depth + 1,
        );
    }
}

fn unwrap_go_generic_callee(mut node: Node) -> Node {
    for _ in 0..8 {
        if matches!(
            node.kind(),
            "index_expression"
                | "index_list_expression"
                | "type_instantiation_expression"
                | "generic_type"
        ) {
            if let Some(op) = node
                .child_by_field_name("operand")
                .or_else(|| node.child_by_field_name("function"))
                .or_else(|| node.child_by_field_name("type"))
                .or_else(|| node.named_child(0))
            {
                node = op;
                continue;
            }
        }
        break;
    }
    node
}

fn extract_go_callee(
    raw_fn: Node,
    content: &str,
    receiver: Option<(&str, &str)>,
    local_types: &HashMap<String, String>,
    imports: &GoImports,
    refs: &mut RefCollector,
) {
    let func_node = unwrap_go_generic_callee(raw_fn);
    if matches!(func_node.kind(), "selector_expression" | "qualified_type") {
        let field_name = if func_node.kind() == "qualified_type" {
            "name"
        } else {
            "field"
        };
        let op_field = if func_node.kind() == "qualified_type" {
            "package"
        } else {
            "operand"
        };
        let func_name = func_node
            .child_by_field_name(field_name)
            .map(|f| node_text(f, content).trim())
            .unwrap_or("");
        if !is_valid_ident(func_name) {
            return;
        }
        if let Some(operand) = func_node.child_by_field_name(op_field) {
            let op_text = node_text(operand, content).trim();
            if imports.external.contains(op_text) {
                // Emit `pkg.Fn` so `external_set` matches `pkg` prefix and NEVER hijacks local `Fn`.
                refs.push_call(&format!("{op_text}.{func_name}"), false);
                return;
            }
            if let Some(hint) = imports.internal.get(op_text) {
                refs.push_call(&super::format_hinted_call(hint, func_name), false);
                return;
            }
            if let Some((_, recv_type)) = receiver.filter(|(id, _)| *id == op_text) {
                refs.push_call(&format!("{recv_type}::{func_name}"), false);
            } else if let Some(ty) = local_types.get(op_text) {
                refs.push_call(&format!("{ty}::{func_name}"), false);
            }
        }
        refs.push_call(func_name, true);
    } else if matches!(func_node.kind(), "identifier" | "type_identifier") {
        let func_name = node_text(func_node, content).trim();
        if is_valid_ident(func_name) {
            refs.push_call(func_name, false);
        }
    }
}

fn extract_go_call(
    call_node: Node,
    content: &str,
    receiver: Option<(&str, &str)>,
    local_types: &HashMap<String, String>,
    imports: &GoImports,
    refs: &mut RefCollector,
) {
    if let Some(raw_fn) = call_node.child_by_field_name("function") {
        extract_go_callee(raw_fn, content, receiver, local_types, imports, refs);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_mentions_collected() {
        let src = r#"
package main

type Config struct {
    Port int
}

func start(c Config) {
    _ = c.Port
}
"#;
        let syms = parse_go("main.go", src, &AliasSet::default()).unwrap();
        let start = syms.iter().find(|s| s.name == "start").unwrap();
        assert!(start.mentions.contains(&"Config".to_string()));
        assert!(start.mentions.contains(&"Port".to_string()));
    }

    #[test]
    fn external_stdlib_calls_stay_qualified_and_never_hijack_local_fns() {
        let src = r#"
package app
import "os"
func Run() { os.Open("x") }
"#;
        let syms = parse_go("caller.go", src, &AliasSet::default()).unwrap();
        let run = syms.iter().find(|s| s.name == "Run").unwrap();
        assert!(run.external_imports.contains(&"os".to_string()));
        assert!(run.calls.contains(&"os.Open".to_string()));
        assert!(!run.calls.contains(&"Open".to_string()));
    }

    #[test]
    fn type_alias_generic_call_and_v2_imports_parse_cleanly() {
        let src = r#"
package app
import "example.com/app/bar/v2"
type MyAlias = string
func Use(x interface{}) {
    GenericFn[int](42)
    bar.DoThing()
}
"#;
        let syms = parse_go("a.go", src, &AliasSet::default()).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.name == "MyAlias" && s.kind == SymbolKind::TypeAlias));
        let u = syms.iter().find(|s| s.name == "Use").unwrap();
        assert!(u.signature.contains("interface{}"), "sig: {}", u.signature);
        assert!(u.calls.contains(&"GenericFn".to_string()), "{:?}", u.calls);
        assert!(
            u.calls.contains(&"bar:DoThing".to_string()),
            "{:?}",
            u.calls
        );
    }
}
