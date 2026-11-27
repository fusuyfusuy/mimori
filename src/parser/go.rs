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

struct GoWalkCtx<'a> {
    content: &'a str,
    file: &'a str,
    file_is_test: bool,
    imports: &'a GoImports,
    depth_truncated: Cell<bool>,
}

impl<'a> GoWalkCtx<'a> {
    fn check_depth(&self, depth: usize) -> bool {
        if depth >= MAX_AST_DEPTH {
            self.depth_truncated.set(true);
            true
        } else {
            false
        }
    }
}

pub fn parse_go(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    Ok(parse_go_with_diagnostics(file, content, aliases)?.0)
}

pub fn parse_go_with_diagnostics(
    file: &str,
    content: &str,
    aliases: &AliasSet,
) -> Result<(Vec<Symbol>, ParseDiagnostics)> {
    let tree = GO_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;

    let root = tree.root_node();
    let depth_truncated = Cell::new(false);
    let imports = collect_go_imports(root, content, aliases, &depth_truncated);
    let file_is_test = file.ends_with("_test.go");
    let ctx = GoWalkCtx {
        content,
        file,
        file_is_test,
        imports: &imports,
        depth_truncated: Cell::new(depth_truncated.get()),
    };

    let mut symbols = Vec::new();
    walk_go_node(root, &ctx, &mut symbols, 0);
    let truncated = ctx.depth_truncated.get();

    let external_imports: Arc<[String]> = imports.external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    let diag = ParseDiagnostics {
        has_error: root.has_error(),
        depth_truncated: truncated,
    };
    Ok((symbols, diag))
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

fn collect_go_imports(
    root: Node,
    content: &str,
    aliases: &AliasSet,
    depth_truncated: &Cell<bool>,
) -> GoImports {
    let mut internal = HashMap::new();
    let mut ext_set = ImportSet::default();
    walk_go_imports(
        root,
        content,
        aliases,
        &mut internal,
        &mut ext_set,
        depth_truncated,
        0,
    );
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
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
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
            // Without `go.mod`, domain-qualified imports (`github.com/...`, `golang.org/...`)
            // and stdlib imports are external; only explicit relative imports are internal.
            let is_internal = if aliases.has_go_modules() {
                aliases.matches_go(p)
            } else {
                p.starts_with("./") || p.starts_with("../")
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
        walk_go_imports(
            c,
            content,
            aliases,
            internal,
            external,
            depth_truncated,
            depth + 1,
        );
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

fn walk_go_node(node: Node, ctx: &GoWalkCtx<'_>, symbols: &mut Vec<Symbol>, depth: usize) {
    if ctx.check_depth(depth) {
        return;
    }
    let content = ctx.content;
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = go_flags(name, ctx.file_is_test, false);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                    flags,
                ));
            }
            return;
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let (recv_ident, receiver_type) = extract_receiver(node, content);
                let full_name = match receiver_type {
                    Some(rt) => format!("{rt}::{name}"),
                    None => name.to_string(),
                };
                let flags = go_flags(name, ctx.file_is_test, true);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::Method,
                    recv_ident.zip(receiver_type),
                    flags,
                ));
            }
            return;
        }
        "type_declaration" => {
            extract_go_type_specs(node, ctx, symbols);
            return;
        }
        "const_declaration" => {
            extract_spec_symbols(node, ctx, "const_spec", SymbolKind::Constant, symbols);
            return;
        }
        "var_declaration" => {
            extract_spec_symbols(node, ctx, "var_spec", SymbolKind::Variable, symbols);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_go_node(child, ctx, symbols, depth + 1);
    }
}

fn extract_go_type_specs(node: Node, ctx: &GoWalkCtx<'_>, symbols: &mut Vec<Symbol>) {
    let content = ctx.content;
    let mut cursor = node.walk();
    let specs: Vec<Node> = node
        .children(&mut cursor)
        .filter(|child| matches!(child.kind(), "type_spec" | "type_alias"))
        .collect();
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
            symbols.push(create_symbol(
                sym_node,
                ctx,
                name.to_string(),
                sym_kind,
                None,
                go_flags(name, ctx.file_is_test, false),
            ));
        }
    }
}

fn go_flags(name: &str, is_test: bool, is_method: bool) -> u8 {
    let mut flags = 0u8;
    if is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        flags |= symbol_flags::IS_PUBLIC;
    }
    if !is_method && name == "init" {
        flags |= symbol_flags::IS_ATTR_ENTRY;
    }
    flags
}

fn extract_spec_symbols(
    decl_node: Node,
    ctx: &GoWalkCtx<'_>,
    spec_kind: &str,
    sym_kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        if child.kind() == spec_kind {
            emit_named_specs(child, ctx, &sym_kind, symbols);
        } else if child.kind().ends_with("_spec_list") {
            let mut lcursor = child.walk();
            for sub in child.children(&mut lcursor) {
                if sub.kind() == spec_kind {
                    emit_named_specs(sub, ctx, &sym_kind, symbols);
                }
            }
        }
    }
}

fn emit_named_specs(
    spec_node: Node,
    ctx: &GoWalkCtx<'_>,
    sym_kind: &SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let mut ncursor = spec_node.walk();
    for name_node in spec_node.children_by_field_name("name", &mut ncursor) {
        let name = node_text(name_node, ctx.content).trim();
        if !name.is_empty() && name != "_" {
            symbols.push(create_symbol(
                spec_node,
                ctx,
                name.to_string(),
                sym_kind.clone(),
                None,
                go_flags(name, ctx.file_is_test, false),
            ));
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

fn is_go_builtin_type(ty: &str) -> bool {
    matches!(
        ty,
        "any"
            | "bool"
            | "byte"
            | "comparable"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
    )
}

fn bind_go_params(fn_node: Node, content: &str, scopes: &mut ScopeStack) {
    let Some(params) = fn_node.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = params.walk();
    for p in params.children(&mut cursor) {
        if p.kind() != "parameter_declaration" {
            continue;
        }
        let ty = p
            .child_by_field_name("type")
            .and_then(|t| strip_go_type_name(node_text(t, content)))
            .filter(|t| !is_go_builtin_type(t));
        let mut ncursor = p.walk();
        for name_node in p.children_by_field_name("name", &mut ncursor) {
            let var = node_text(name_node, content).trim();
            if is_valid_ident(var) {
                if let Some(ty_name) = ty {
                    scopes.insert(var.to_string(), ty_name.to_string());
                } else {
                    scopes.shadow(var.to_string());
                }
            }
        }
    }
}

fn infer_go_expr_type(mut expr: Node, content: &str) -> Option<String> {
    for _ in 0..4 {
        match expr.kind() {
            "unary_expression" | "parenthesized_expression" => {
                expr = expr
                    .child_by_field_name("operand")
                    .or_else(|| expr.named_child(0))?;
            }
            "composite_literal" => {
                let ty = expr.child_by_field_name("type")?;
                let name = strip_go_type_name(node_text(ty, content))?;
                return (!is_go_builtin_type(name)).then(|| name.to_string());
            }
            _ => return None,
        }
    }
    None
}

fn bind_go_short_var_decl(node: Node, content: &str, scopes: &mut ScopeStack) {
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return;
    };
    let mut lcur = left.walk();
    let lhs: Vec<Node> = left.named_children(&mut lcur).collect();
    let mut rcur = right.walk();
    let rhs: Vec<Node> = right.named_children(&mut rcur).collect();
    for (i, l) in lhs.iter().enumerate() {
        if l.kind() != "identifier" {
            continue;
        }
        let var = node_text(*l, content).trim();
        if !is_valid_ident(var) || var == "_" {
            continue;
        }
        if let Some(ty) = rhs.get(i).and_then(|r| infer_go_expr_type(*r, content)) {
            scopes.insert(var.to_string(), ty);
        } else {
            scopes.shadow(var.to_string());
        }
    }
}

fn bind_go_var_spec(node: Node, content: &str, scopes: &mut ScopeStack) {
    let explicit_ty = node
        .child_by_field_name("type")
        .and_then(|t| strip_go_type_name(node_text(t, content)))
        .filter(|t| !is_go_builtin_type(t))
        .map(str::to_string);
    let inferred_ty = explicit_ty.or_else(|| {
        node.child_by_field_name("value")
            .and_then(|v| v.named_child(0).or(Some(v)))
            .and_then(|e| infer_go_expr_type(e, content))
    });
    let mut ncursor = node.walk();
    for name_node in node.children_by_field_name("name", &mut ncursor) {
        let var = node_text(name_node, content).trim();
        if !is_valid_ident(var) || var == "_" {
            continue;
        }
        if let Some(ref ty) = inferred_ty {
            scopes.insert(var.to_string(), ty.clone());
        } else {
            scopes.shadow(var.to_string());
        }
    }
}

fn create_symbol(
    node: Node,
    ctx: &GoWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    receiver: Option<(&str, &str)>,
    flags: u8,
) -> Symbol {
    let content = ctx.content;
    let raw_body = node_text(node, content);
    let first_line = raw_body.lines().next().unwrap_or("").trim();
    let signature = if matches!(kind, SymbolKind::Constant | SymbolKind::Variable) {
        first_line.to_string()
    } else {
        extract_signature_ast(node, content, first_line)
    };

    let mut scopes = ScopeStack::new();
    if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        bind_go_params(node, content, &mut scopes);
    }

    let mut refs = RefCollector::default();
    collect_references(node, ctx, receiver, &mut scopes, &mut refs, 0);

    build_symbol(node, ctx.file, name, kind, signature, refs, flags)
}

fn collect_references(
    node: Node,
    ctx: &GoWalkCtx<'_>,
    receiver: Option<(&str, &str)>,
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
        match kind {
            "block" => {
                scopes.push();
                collect_references(child, ctx, receiver, scopes, refs, depth + 1);
                scopes.pop();
                continue;
            }
            "func_literal" => {
                scopes.push();
                bind_go_params(child, content, scopes);
                collect_references(child, ctx, receiver, scopes, refs, depth + 1);
                scopes.pop();
                continue;
            }
            "short_var_declaration" => {
                collect_references(child, ctx, receiver, scopes, refs, depth + 1);
                bind_go_short_var_decl(child, content, scopes);
                continue;
            }
            "var_spec" => {
                collect_references(child, ctx, receiver, scopes, refs, depth + 1);
                bind_go_var_spec(child, content, scopes);
                continue;
            }
            "call_expression" => {
                extract_go_call(child, ctx, receiver, scopes, refs);
            }
            "type_conversion_expression" => {
                // tree-sitter-go parses single-argument generic calls (`GenericFn[int](42)`
                // and `pkg.GenericFn[int](42)`) as `type_conversion_expression` with a
                // `generic_type` or `type_instantiation_expression` target.
                if let Some(ty) = child.child_by_field_name("type") {
                    if matches!(ty.kind(), "generic_type" | "type_instantiation_expression") {
                        extract_go_callee(ty, ctx, receiver, scopes, refs);
                    }
                }
            }
            "selector_expression" => {
                if let Some(field) = child.child_by_field_name("field") {
                    refs.push_mention(node_text(field, content));
                }
            }
            "type_identifier" => {
                refs.push_mention(node_text(child, content));
            }
            _ => {}
        }

        collect_references(child, ctx, receiver, scopes, refs, depth + 1);
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
    ctx: &GoWalkCtx<'_>,
    receiver: Option<(&str, &str)>,
    scopes: &ScopeStack,
    refs: &mut RefCollector,
) {
    let content = ctx.content;
    let imports = ctx.imports;
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
                refs.push_call(&format!("{op_text}.{func_name}"), false);
                return;
            }
            if let Some(hint) = imports.internal.get(op_text) {
                refs.push_call(&super::format_hinted_call(hint, func_name), false);
                return;
            }
            if let Some((_, recv_type)) = receiver.filter(|(id, _)| *id == op_text) {
                refs.push_call(&format!("{recv_type}::{func_name}"), false);
                return;
            }
            if let Some(ty) = scopes.get(op_text) {
                refs.push_call(&format!("{ty}::{func_name}"), false);
                return;
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
    ctx: &GoWalkCtx<'_>,
    receiver: Option<(&str, &str)>,
    scopes: &ScopeStack,
    refs: &mut RefCollector,
) {
    if let Some(raw_fn) = call_node.child_by_field_name("function") {
        extract_go_callee(raw_fn, ctx, receiver, scopes, refs);
    }
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut acursor = args_node.walk();
        for arg in args_node.children(&mut acursor) {
            if arg.kind() == "identifier" {
                refs.push_mention(node_text(arg, ctx.content));
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
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/app\n").unwrap();
        let aliases = AliasSet::collect(dir.path());
        let src = r#"
package app
import "example.com/app/bar/v2"
type MyAlias = string
func Use(x interface{}) {
    GenericFn[int](42)
    bar.DoThing()
}
"#;
        let syms = parse_go("a.go", src, &aliases).unwrap();
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

    #[test]
    fn go_flags_unexported_receivers_and_no_gomod_externals() {
        let src = r#"
package p
import "github.com/gin-gonic/gin"

type server struct{}
func (s *server) stop() {}
func TestConnection(s *server) {
    s.stop()
    g := &server{}
    g.stop()
    gin.New()
}
func init() {}
"#;
        let syms = parse_go("h.go", src, &AliasSet::default()).unwrap();
        let tc = syms.iter().find(|s| s.name == "TestConnection").unwrap();
        assert!(!tc.is_test_flag());
        assert!(tc.is_public());
        assert!(
            tc.calls.contains(&"server::stop".to_string()),
            "{:?}",
            tc.calls
        );
        assert!(
            !tc.member_calls.contains(&"stop".to_string()),
            "{:?}",
            tc.member_calls
        );
        assert!(tc.calls.contains(&"gin.New".to_string()), "{:?}", tc.calls);
        assert!(tc.external_imports.contains(&"gin".to_string()));
        let init_fn = syms.iter().find(|s| s.name == "init").unwrap();
        assert!(init_fn.is_attr_entry());
        assert!(!init_fn.is_test_flag());
    }
}
