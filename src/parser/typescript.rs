use crate::model::{symbol_flags, Symbol, SymbolKind};
use crate::parser::common::{
    build_symbol, extract_signature_ast, is_valid_ident, node_text, ImportSet, ParseDiagnostics,
    RefCollector, ScopeStack, MAX_AST_DEPTH,
};
use crate::workspace::AliasSet;
use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tree_sitter::{Node, Parser};

thread_local! {
    static TS_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("Error loading TypeScript grammar");
        p
    });
    static TSX_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .expect("Error loading TSX grammar");
        p
    });
}

struct TsWalkCtx<'a> {
    content: &'a str,
    file: &'a str,
    file_is_test: bool,
    aliases: &'a AliasSet,
    external_set: &'a HashSet<String>,
    internal_namespaces: &'a HashMap<String, String>,
    post_exported: &'a HashSet<String>,
    depth_truncated: Cell<bool>,
}

impl<'a> TsWalkCtx<'a> {
    fn check_depth(&self, depth: usize) -> bool {
        if depth >= MAX_AST_DEPTH {
            self.depth_truncated.set(true);
            true
        } else {
            false
        }
    }
}

pub fn parse_typescript(
    file: &str,
    content: &str,
    is_tsx: bool,
    aliases: &AliasSet,
) -> Result<Vec<Symbol>> {
    Ok(parse_typescript_with_diagnostics(file, content, is_tsx, aliases)?.0)
}

pub fn parse_typescript_with_diagnostics(
    file: &str,
    content: &str,
    is_tsx: bool,
    aliases: &AliasSet,
) -> Result<(Vec<Symbol>, ParseDiagnostics)> {
    let tree = if is_tsx {
        TSX_PARSER.with(|p| p.borrow_mut().parse(content, None))
    } else {
        TS_PARSER.with(|p| p.borrow_mut().parse(content, None))
    }
    .ok_or_else(|| anyhow::anyhow!("Failed to parse TypeScript source"))?;

    let root = tree.root_node();
    let depth_truncated = Cell::new(false);
    let external_list = collect_external_imports(root, content, aliases, &depth_truncated);
    let external_set: HashSet<String> = external_list.iter().cloned().collect();
    let (import_aliases, internal_namespaces) =
        collect_local_imports(root, content, aliases, &depth_truncated);
    let post_exported = collect_post_exports(root, content);

    let file_is_test = file.starts_with("tests/")
        || file.contains("/tests/")
        || file.contains("/__tests__/")
        || file.contains(".test.")
        || file.contains(".spec.");

    let ctx = TsWalkCtx {
        content,
        file,
        file_is_test,
        aliases,
        external_set: &external_set,
        internal_namespaces: &internal_namespaces,
        post_exported: &post_exported,
        depth_truncated: Cell::new(depth_truncated.get()),
    };

    let mut symbols = Vec::new();
    walk_ts_node(root, &ctx, None, false, &[], &mut symbols, 0);

    super::apply_import_aliases(&mut symbols, &import_aliases);

    let external_imports: Arc<[String]> = external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    let diag = ParseDiagnostics {
        has_error: root.has_error(),
        depth_truncated: ctx.depth_truncated.get(),
    };
    Ok((symbols, diag))
}

fn unwrap_ts_expr(mut node: Node) -> Node {
    for _ in 0..16 {
        match node.kind() {
            "satisfies_expression"
            | "as_expression"
            | "parenthesized_expression"
            | "non_null_expression" => {
                let Some(inner) = node.named_child(0) else {
                    break;
                };
                node = inner;
            }
            "type_assertion" => {
                let inner = (0..node.named_child_count())
                    .rev()
                    .find_map(|i| node.named_child(i).filter(|c| c.kind() != "type_arguments"));
                let Some(inner) = inner else {
                    break;
                };
                node = inner;
            }
            _ => break,
        }
    }
    node
}

fn is_top_level_declarator(declarator: Node) -> bool {
    let Some(decl) = declarator.parent() else {
        return false;
    };
    if !matches!(decl.kind(), "lexical_declaration" | "variable_declaration") {
        return false;
    }
    let Some(gp) = decl.parent() else {
        return false;
    };
    gp.kind() == "program"
        || (gp.kind() == "export_statement"
            && gp.parent().is_some_and(|ggp| ggp.kind() == "program"))
}

fn has_own_or_export_decorator(node: Node) -> bool {
    let mut cursor = node.walk();
    if node.children(&mut cursor).any(|c| c.kind() == "decorator") {
        return true;
    }
    if let Some(parent) = node.parent().filter(|p| p.kind() == "export_statement") {
        let mut pcur = parent.walk();
        if parent.children(&mut pcur).any(|c| c.kind() == "decorator") {
            return true;
        }
    }
    false
}

fn is_private_or_protected_member(node: Node, content: &str) -> bool {
    if let Some(name_node) = node.child_by_field_name("name") {
        if name_node.kind() == "private_property_identifier"
            || node_text(name_node, content).trim().starts_with('#')
        {
            return true;
        }
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "accessibility_modifier" {
            let m = node_text(c, content).trim();
            if m == "private" || m == "protected" {
                return true;
            }
        }
    }
    false
}

fn ts_top_flags(node: Node, name: &str, ctx: &TsWalkCtx<'_>, has_decorator: bool) -> u8 {
    let mut flags = 0u8;
    if ctx.file_is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if is_exported_ts_node(node) || ctx.post_exported.contains(name) {
        flags |= symbol_flags::IS_PUBLIC;
    }
    if has_decorator || has_own_or_export_decorator(node) {
        flags |= symbol_flags::IS_ATTR_ENTRY;
    }
    flags
}

fn ts_member_flags(
    node: Node,
    parent_exported: bool,
    has_decorator: bool,
    ctx: &TsWalkCtx<'_>,
) -> u8 {
    let mut flags = 0u8;
    if ctx.file_is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if parent_exported && !is_private_or_protected_member(node, ctx.content) {
        flags |= symbol_flags::IS_PUBLIC;
    }
    if has_decorator || has_own_or_export_decorator(node) {
        flags |= symbol_flags::IS_ATTR_ENTRY;
    }
    flags
}

fn collect_post_exports(root: Node, content: &str) -> HashSet<String> {
    let mut exported = HashSet::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() != "export_statement" || child.child_by_field_name("source").is_some() {
            continue;
        }
        let mut ccur = child.walk();
        for item in child.children(&mut ccur) {
            match item.kind() {
                "export_clause" => {
                    let mut ecur = item.walk();
                    for spec in item.children(&mut ecur) {
                        if spec.kind() == "export_specifier" {
                            if let Some(name_node) = spec.child_by_field_name("name") {
                                let name = node_text(name_node, content).trim();
                                if is_valid_ident(name) {
                                    exported.insert(name.to_string());
                                }
                            }
                        }
                    }
                }
                "identifier" => {
                    // `export default foo;`
                    let name = node_text(item, content).trim();
                    if is_valid_ident(name) {
                        exported.insert(name.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    exported
}

fn walk_ts_node<'a>(
    node: Node<'a>,
    ctx: &TsWalkCtx<'a>,
    parent_class: Option<&'a str>,
    parent_exported: bool,
    pending_decorators: &[Node<'a>],
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let content = ctx.content;
    let kind = node.kind();
    let has_dec = !pending_decorators.is_empty();

    match kind {
        "export_statement" if node.child_by_field_name("source").is_some() => {
            extract_reexport_symbols(node, ctx, symbols);
            return;
        }
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = ts_top_flags(node, name, ctx, has_dec);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                    flags,
                    pending_decorators,
                ));
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let flags = ts_top_flags(node, name, ctx, has_dec);
                let class_exported = (flags & symbol_flags::IS_PUBLIC) != 0;
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Class,
                    Some(name),
                    flags,
                    pending_decorators,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    walk_ts_children(body, ctx, Some(name), class_exported, symbols, depth + 1);
                }
                return;
            }
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let sym_kind = match kind {
                    "interface_declaration" => SymbolKind::Interface,
                    "type_alias_declaration" => SymbolKind::TypeAlias,
                    _ => SymbolKind::Enum,
                };
                let flags = ts_top_flags(node, name, ctx, has_dec);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    sym_kind,
                    None,
                    flags,
                    pending_decorators,
                ));
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_class {
                    Some(pc) => format!("{pc}::{name}"),
                    None => name.to_string(),
                };
                let flags = ts_member_flags(node, parent_exported, has_dec, ctx);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::Method,
                    parent_class,
                    flags,
                    pending_decorators,
                ));
                if name == "constructor" {
                    if let Some(pc) = parent_class {
                        extract_ctor_param_fields(node, ctx, pc, parent_exported, symbols);
                    }
                }
            }
        }
        "variable_declarator" => {
            if handle_variable_declarator(node, ctx, symbols) {
                return;
            }
        }
        _ if kind.ends_with("field_definition") => {
            if let Some(pc) = parent_class {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fname = node_text(name_node, content).trim();
                    if !fname.is_empty() {
                        let full_name = format!("{pc}::{fname}");
                        let flags = ts_member_flags(node, parent_exported, has_dec, ctx);
                        symbols.push(create_symbol(
                            node,
                            ctx,
                            full_name,
                            SymbolKind::Field,
                            Some(pc),
                            flags,
                            pending_decorators,
                        ));
                    }
                }
            }
        }
        _ => {}
    }

    let next_parent = if matches!(
        kind,
        "method_definition" | "function_declaration" | "generator_function_declaration"
    ) || kind.ends_with("field_definition")
    {
        None
    } else {
        parent_class
    };
    let next_exported = if next_parent.is_some() {
        parent_exported
    } else {
        false
    };
    walk_ts_children(node, ctx, next_parent, next_exported, symbols, depth + 1);
}

fn walk_ts_children<'a>(
    parent: Node<'a>,
    ctx: &TsWalkCtx<'a>,
    parent_class: Option<&'a str>,
    parent_exported: bool,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let mut pending_decorators: Vec<Node<'a>> = Vec::new();
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            "decorator" => {
                pending_decorators.push(child);
            }
            "comment" => {}
            _ => {
                walk_ts_node(
                    child,
                    ctx,
                    parent_class,
                    parent_exported,
                    &pending_decorators,
                    symbols,
                    depth + 1,
                );
                pending_decorators.clear();
            }
        }
    }
}

fn has_template_substitution(node: Node) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.kind() == "template_substitution");
    found
}

fn handle_variable_declarator(node: Node, ctx: &TsWalkCtx<'_>, symbols: &mut Vec<Symbol>) -> bool {
    let content = ctx.content;
    let (Some(name_node), Some(raw_val_node)) = (
        node.child_by_field_name("name"),
        node.child_by_field_name("value"),
    ) else {
        return false;
    };

    let top_level = is_top_level_declarator(node);

    if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
        if top_level {
            let mut names = Vec::new();
            extract_pattern_names(name_node, content, &mut names, &ctx.depth_truncated, 0);
            for pname in names {
                let flags = ts_top_flags(node, pname, ctx, false);
                symbols.push(create_symbol(
                    node,
                    ctx,
                    pname.to_string(),
                    SymbolKind::Variable,
                    None,
                    flags,
                    &[],
                ));
            }
        }
        return true;
    }

    let name = node_text(name_node, content).trim();
    let flags = if top_level {
        ts_top_flags(node, name, ctx, false)
    } else if ctx.file_is_test {
        symbol_flags::IS_TEST
    } else {
        0
    };
    let is_exported = (flags & symbol_flags::IS_PUBLIC) != 0;
    let value_node = unwrap_ts_expr(raw_val_node);
    let val_kind = value_node.kind();

    if matches!(
        val_kind,
        "arrow_function" | "function" | "function_expression" | "generator_function"
    ) && top_level
    {
        symbols.push(create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Function,
            None,
            flags,
            &[],
        ));
    } else if val_kind == "object" && top_level {
        let sym_kind = if is_top_level_const(node, content) {
            SymbolKind::Constant
        } else {
            SymbolKind::Variable
        };
        let mut members = Vec::new();
        let mut skip_ids = HashSet::new();
        extract_object_literal_members(
            value_node,
            ctx,
            name,
            is_exported,
            &mut members,
            &mut skip_ids,
        );
        symbols.push(create_symbol_skipping(
            node,
            ctx,
            name.to_string(),
            sym_kind,
            None,
            flags,
            &skip_ids,
            &[],
        ));
        symbols.extend(members);
        return true;
    } else if val_kind == "call_expression" && top_level {
        if let Some(args_node) = value_node.child_by_field_name("arguments") {
            let mut members = Vec::new();
            let mut skip_ids = HashSet::new();
            let mut arg_cursor = args_node.walk();
            for arg in args_node.children(&mut arg_cursor) {
                let unwrapped_arg = unwrap_ts_expr(arg);
                if unwrapped_arg.kind() == "object" {
                    extract_object_literal_members(
                        unwrapped_arg,
                        ctx,
                        name,
                        is_exported,
                        &mut members,
                        &mut skip_ids,
                    );
                }
            }
            symbols.push(create_symbol_skipping(
                node,
                ctx,
                name.to_string(),
                SymbolKind::Variable,
                None,
                flags,
                &skip_ids,
                &[],
            ));
            symbols.extend(members);
            return true;
        }
        symbols.push(create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Variable,
            None,
            flags,
            &[],
        ));
    } else if val_kind == "template_string" && (top_level || has_template_substitution(value_node))
    {
        // Emit top-level template bindings or function-local interpolated templates
        // (`const cmd = `... ${x} ...``) for `up`/`uses` dataflow without leaking
        // plain static local backtick strings (`const msg = `hi``).
        let sym_kind = if top_level && is_top_level_const(node, content) {
            SymbolKind::Constant
        } else {
            SymbolKind::Variable
        };
        symbols.push(create_symbol(
            node,
            ctx,
            name.to_string(),
            sym_kind,
            None,
            flags,
            &[],
        ));
    } else if is_top_level_const(node, content) {
        symbols.push(create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Constant,
            None,
            flags,
            &[],
        ));
    }
    false
}

fn extract_reexport_symbols(node: Node, ctx: &TsWalkCtx<'_>, symbols: &mut Vec<Symbol>) {
    let content = ctx.content;
    let Some(source) = node.child_by_field_name("source") else {
        return;
    };
    let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
    let is_local = spec.starts_with('.') || spec.starts_with('/') || ctx.aliases.matches(spec);
    if !is_local {
        return;
    }
    let hint = super::spec_to_module_hint(spec, ctx.aliases);
    let stmt_is_type = node_text(node, content)
        .trim_start()
        .starts_with("export type ");
    collect_reexport_specifiers(node, ctx, &hint, stmt_is_type, symbols, 0);
}

fn collect_reexport_specifiers(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    hint: &str,
    stmt_is_type: bool,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let content = ctx.content;
    match node.kind() {
        "export_specifier" => {
            if let Some(orig_node) = node.child_by_field_name("name") {
                let orig = node_text(orig_node, content).trim();
                let alias = node
                    .child_by_field_name("alias")
                    .map(|a| node_text(a, content).trim())
                    .filter(|a| !a.is_empty())
                    .unwrap_or(orig);
                if !orig.is_empty() && !alias.is_empty() {
                    let is_type =
                        stmt_is_type || node_text(node, content).trim_start().starts_with("type ");
                    let kind = if is_type {
                        SymbolKind::TypeAlias
                    } else {
                        SymbolKind::Function
                    };
                    let mut calls = Vec::new();
                    let mut mentions = Vec::new();
                    let mut call_counts = BTreeMap::new();
                    if is_type {
                        mentions.push(orig.to_string());
                    } else {
                        let target = super::format_hinted_call(hint, orig);
                        call_counts.insert(target.clone(), 1);
                        calls.push(target);
                    }
                    let mut flags = symbol_flags::IS_PUBLIC;
                    if ctx.file_is_test {
                        flags |= symbol_flags::IS_TEST;
                    }
                    let sig = if orig == alias {
                        format!("export {{ {orig} }}")
                    } else {
                        format!("export {{ {orig} as {alias} }}")
                    };
                    let start_pos = node.start_position();
                    let end_pos = node.end_position();
                    symbols.push(Symbol {
                        name: alias.to_string(),
                        kind,
                        file: ctx.file.to_string(),
                        start_line: start_pos.row + 1,
                        end_line: end_pos.row + 1,
                        signature: sig,
                        body: String::new(),
                        centrality: 0.0,
                        calls,
                        mentions,
                        call_counts,
                        member_calls: Vec::new(),
                        external_imports: crate::model::symbol::empty_external_imports(),
                        flags,
                    });
                }
            }
            return;
        }
        "namespace_export" => {
            if let Some(id) = node.named_child(0) {
                let alias = node_text(id, content).trim();
                if is_valid_ident(alias) {
                    let mut flags = symbol_flags::IS_PUBLIC;
                    if ctx.file_is_test {
                        flags |= symbol_flags::IS_TEST;
                    }
                    let mut refs = RefCollector::default();
                    if !hint.is_empty() {
                        refs.push_mention(hint);
                    }
                    symbols.push(build_symbol(
                        node,
                        ctx.file,
                        alias.to_string(),
                        SymbolKind::Module,
                        format!("export * as {alias}"),
                        refs,
                        flags,
                    ));
                }
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_reexport_specifiers(c, ctx, hint, stmt_is_type, symbols, depth + 1);
    }
}

fn extract_ctor_param_fields(
    ctor_node: Node,
    ctx: &TsWalkCtx<'_>,
    parent_class: &str,
    parent_exported: bool,
    symbols: &mut Vec<Symbol>,
) {
    let content = ctx.content;
    let Some(params) = ctor_node.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = params.walk();
    for param in params.children(&mut cursor) {
        let pk = param.kind();
        if pk != "required_parameter" && pk != "optional_parameter" {
            continue;
        }
        let mut has_modifier = false;
        let mut pcursor = param.walk();
        for c in param.children(&mut pcursor) {
            if c.kind() == "accessibility_modifier" || c.kind() == "readonly" {
                has_modifier = true;
                break;
            }
        }
        if !has_modifier {
            continue;
        }
        if let Some(pat) = param.child_by_field_name("pattern") {
            let fname = node_text(pat, content).trim();
            if !fname.is_empty() {
                let full_name = format!("{parent_class}::{fname}");
                let flags = ts_member_flags(param, parent_exported, false, ctx);
                symbols.push(create_symbol(
                    param,
                    ctx,
                    full_name,
                    SymbolKind::Field,
                    Some(parent_class),
                    flags,
                    &[],
                ));
            }
        }
    }
}

fn is_exported_ts_node(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "export_statement"
        || (matches!(
            parent.kind(),
            "lexical_declaration" | "variable_declaration"
        ) && parent
            .parent()
            .is_some_and(|gp| gp.kind() == "export_statement"))
}

fn bind_ts_function_params(fn_node: Node, content: &str, scopes: &mut ScopeStack) {
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for p in params.children(&mut cursor) {
            if matches!(p.kind(), "required_parameter" | "optional_parameter") {
                if let Some(pat) = p.child_by_field_name("pattern") {
                    let var = node_text(pat, content).trim();
                    if is_valid_ident(var) {
                        if let Some(ty) = p
                            .child_by_field_name("type")
                            .and_then(|t| extract_ts_type_ident(t, content))
                        {
                            scopes.insert(var.to_string(), ty);
                        } else {
                            scopes.shadow(var.to_string());
                        }
                    }
                }
            } else if p.kind() == "identifier" {
                let var = node_text(p, content).trim();
                if is_valid_ident(var) {
                    scopes.shadow(var.to_string());
                }
            }
        }
    } else if let Some(single) = fn_node.child_by_field_name("parameter") {
        let var = node_text(single, content).trim();
        if is_valid_ident(var) {
            scopes.shadow(var.to_string());
        }
    }
}

fn init_ts_symbol_scope(node: Node, content: &str) -> ScopeStack {
    let mut scopes = ScopeStack::new();
    let fn_node = if node.kind() == "variable_declarator" {
        node.child_by_field_name("value")
            .map(unwrap_ts_expr)
            .unwrap_or(node)
    } else {
        node
    };
    bind_ts_function_params(fn_node, content, &mut scopes);
    scopes
}

fn infer_ts_var_type(declarator: Node, content: &str) -> Option<String> {
    declarator
        .child_by_field_name("type")
        .and_then(|t| extract_ts_type_ident(t, content))
        .or_else(|| {
            declarator.child_by_field_name("value").and_then(|v| {
                let u = unwrap_ts_expr(v);
                if u.kind() == "new_expression" {
                    u.child_by_field_name("constructor").and_then(|c| {
                        let t = node_text(c, content)
                            .rsplit('.')
                            .next()
                            .unwrap_or("")
                            .trim();
                        (is_valid_ident(t)
                            && t.starts_with(|ch: char| ch.is_ascii_uppercase())
                            && !is_ts_builtin_type(t))
                        .then(|| t.to_string())
                    })
                } else {
                    None
                }
            })
        })
}

fn is_ts_builtin_type(t: &str) -> bool {
    matches!(
        t,
        "Array"
            | "Promise"
            | "Record"
            | "Map"
            | "Set"
            | "WeakMap"
            | "WeakSet"
            | "Error"
            | "Object"
            | "Function"
            | "String"
            | "Number"
            | "Boolean"
            | "Date"
            | "RegExp"
            | "Symbol"
            | "BigInt"
            | "Math"
            | "JSON"
            | "Reflect"
            | "Proxy"
            | "Intl"
            | "Buffer"
            | "URL"
            | "Uint8Array"
    )
}

fn extract_ts_type_ident(node: Node, content: &str) -> Option<String> {
    let mut cur = node;
    for _ in 0..8 {
        match cur.kind() {
            "type_annotation" | "parenthesized_type" => cur = cur.named_child(0)?,
            "generic_type" => cur = cur.child_by_field_name("name")?,
            "union_type" => {
                // Unwrap `Foo | null` / `Foo | undefined` / `Foo | null | undefined`.
                let mut cursor = cur.walk();
                let mut found: Option<String> = None;
                for c in cur.named_children(&mut cursor) {
                    let txt = node_text(c, content).trim();
                    if matches!(txt, "null" | "undefined" | "void")
                        || matches!(c.kind(), "null" | "undefined")
                    {
                        continue;
                    }
                    let ty = extract_ts_type_ident(c, content)?;
                    if found.is_some() {
                        return None;
                    }
                    found = Some(ty);
                }
                return found;
            }
            "type_identifier" => {
                let t = node_text(cur, content).trim();
                return (is_valid_ident(t)
                    && t.starts_with(|c: char| c.is_ascii_uppercase())
                    && !is_ts_builtin_type(t))
                .then(|| t.to_string());
            }
            _ => return None,
        }
    }
    None
}

fn create_symbol(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    enclosing_class: Option<&str>,
    flags: u8,
    decorators: &[Node],
) -> Symbol {
    let empty = HashSet::new();
    create_symbol_skipping(
        node,
        ctx,
        name,
        kind,
        enclosing_class,
        flags,
        &empty,
        decorators,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_symbol_skipping(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    enclosing_class: Option<&str>,
    flags: u8,
    skip_ids: &HashSet<usize>,
    decorators: &[Node],
) -> Symbol {
    let content = ctx.content;
    let raw_body = node_text(node, content);
    let first_line = raw_body.lines().next().unwrap_or("").trim();
    let raw_sig = match kind {
        SymbolKind::Constant | SymbolKind::Variable => first_line.to_string(),
        _ => {
            let ast_sig = extract_signature_ast(node, content, first_line);
            if ast_sig.len() < 160 {
                ast_sig
            } else {
                first_line.to_string()
            }
        }
    };
    let signature = if is_exported_ts_node(node) && !raw_sig.starts_with("export ") {
        format!("export {raw_sig}")
    } else {
        raw_sig
    };

    let mut scopes = if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        init_ts_symbol_scope(node, content)
    } else {
        ScopeStack::new()
    };

    let mut refs = RefCollector::default();
    let root_fn_id = if node.kind() == "variable_declarator" {
        node.child_by_field_name("value")
            .map(unwrap_ts_expr)
            .filter(|v| {
                matches!(
                    v.kind(),
                    "arrow_function" | "function" | "function_expression" | "generator_function"
                )
            })
            .map(|v| v.id())
    } else {
        None
    };

    for &dec in decorators {
        visit_ts_ref(
            dec,
            ctx,
            enclosing_class,
            &mut scopes,
            false,
            skip_ids,
            root_fn_id,
            &mut refs,
            0,
        );
    }

    collect_references(
        node,
        ctx,
        enclosing_class,
        &mut scopes,
        false,
        skip_ids,
        root_fn_id,
        &mut refs,
        0,
    );

    build_symbol(node, ctx.file, name, kind, signature, refs, flags)
}

#[allow(clippy::too_many_arguments)]
fn collect_references(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    scopes: &mut ScopeStack,
    in_value_pos: bool,
    skip_ids: &HashSet<usize>,
    root_fn_id: Option<usize>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if ctx.check_depth(depth) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_ts_ref(
            child,
            ctx,
            enclosing_class,
            scopes,
            in_value_pos,
            skip_ids,
            root_fn_id,
            refs,
            depth + 1,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_ts_ref(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    scopes: &mut ScopeStack,
    in_value_pos: bool,
    skip_ids: &HashSet<usize>,
    root_fn_id: Option<usize>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if ctx.check_depth(depth) || skip_ids.contains(&node.id()) {
        return;
    }
    let content = ctx.content;
    let kind = node.kind();
    if matches!(
        kind,
        "method_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "function_declaration"
            | "generator_function_declaration"
    ) || kind.ends_with("field_definition")
    {
        return;
    }

    match kind {
        "statement_block" => {
            scopes.push();
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
            scopes.pop();
        }
        "arrow_function" | "function" | "function_expression" | "generator_function"
            if Some(node.id()) != root_fn_id =>
        {
            scopes.push();
            bind_ts_function_params(node, content, scopes);
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
            scopes.pop();
        }
        "variable_declarator" => {
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
            if let Some(name_node) = node.child_by_field_name("name") {
                let var = node_text(name_node, content).trim();
                if is_valid_ident(var) {
                    if let Some(ty) = infer_ts_var_type(node, content) {
                        scopes.insert(var.to_string(), ty);
                    } else {
                        scopes.shadow(var.to_string());
                    }
                }
            }
        }
        "call_expression" => {
            visit_ts_call(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        }
        "new_expression" => {
            visit_ts_new(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        }
        "jsx_self_closing_element" | "jsx_opening_element" => {
            if let Some(tag_node) = node.child_by_field_name("name") {
                let tag = node_text(tag_node, content).trim();
                let root_tag = tag.split('.').next().unwrap_or(tag);
                if root_tag.starts_with(|c: char| c.is_ascii_uppercase()) {
                    if let Some((_, prop)) = tag.rsplit_once('.') {
                        refs.push_mention(root_tag);
                        refs.push_mention(prop);
                        refs.push_call(tag, false);
                    } else if is_valid_ident(tag) {
                        refs.push_mention(tag);
                        refs.push_call(tag, false);
                    }
                }
            }
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        }
        "jsx_expression" | "template_substitution" => {
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                true,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        }
        "member_expression" => {
            if let Some(prop) = node.child_by_field_name("property") {
                let p = node_text(prop, content).trim();
                if p != "this" && p != "super" {
                    refs.push_mention(p);
                }
            }
            if let Some(obj) = node.child_by_field_name("object") {
                visit_ts_ref(
                    obj,
                    ctx,
                    enclosing_class,
                    scopes,
                    in_value_pos,
                    skip_ids,
                    root_fn_id,
                    refs,
                    depth + 1,
                );
            }
        }
        "type_identifier" => {
            refs.push_mention(node_text(node, content).trim());
        }
        "identifier" => {
            if in_value_pos {
                let t = node_text(node, content).trim();
                if t != "this" && t != "super" {
                    refs.push_mention(t);
                }
            }
        }
        _ => {
            collect_references(
                node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_ts_new(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    scopes: &mut ScopeStack,
    in_value_pos: bool,
    skip_ids: &HashSet<usize>,
    root_fn_id: Option<usize>,
    refs: &mut RefCollector,
    depth: usize,
) {
    let ctor_node = node.child_by_field_name("constructor");
    if let Some(ctor) = ctor_node {
        let text = node_text(ctor, ctx.content);
        let short = text.rsplit(['.', ':']).next().unwrap_or(text).trim();
        if !short.is_empty() && short != "super" && short != "this" {
            refs.push_call(short, false);
            refs.push_call(&format!("{short}::constructor"), false);
        }
    }
    let ctor_id = ctor_node.map(|c| c.id());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == ctor_id && !in_value_pos {
            continue;
        }
        let child_val = in_value_pos || child.kind() == "arguments";
        visit_ts_ref(
            child,
            ctx,
            enclosing_class,
            scopes,
            child_val,
            skip_ids,
            root_fn_id,
            refs,
            depth + 1,
        );
    }
}

fn is_ts_static_class_ident(s: &str) -> bool {
    is_valid_ident(s) && s.starts_with(|c: char| c.is_ascii_uppercase()) && !is_ts_builtin_type(s)
}

#[allow(clippy::too_many_arguments)]
fn visit_ts_call(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    scopes: &mut ScopeStack,
    in_value_pos: bool,
    skip_ids: &HashSet<usize>,
    root_fn_id: Option<usize>,
    refs: &mut RefCollector,
    depth: usize,
) {
    let content = ctx.content;
    let func_node = node.child_by_field_name("function");
    if let Some(raw_fn) = func_node {
        let fn_node = unwrap_ts_expr(raw_fn);
        if fn_node.kind() == "member_expression" {
            extract_ts_member_call(
                fn_node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth,
            );
        } else if fn_node.kind() == "identifier" {
            let t = node_text(fn_node, content).trim();
            if t != "this" && t != "super" {
                refs.push_call(t, false);
            }
        } else {
            visit_ts_ref(
                fn_node,
                ctx,
                enclosing_class,
                scopes,
                in_value_pos,
                skip_ids,
                root_fn_id,
                refs,
                depth + 1,
            );
        }
    }
    let fn_id = func_node.map(|f| f.id());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == fn_id {
            continue;
        }
        let child_val = in_value_pos || child.kind() == "arguments";
        visit_ts_ref(
            child,
            ctx,
            enclosing_class,
            scopes,
            child_val,
            skip_ids,
            root_fn_id,
            refs,
            depth + 1,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_ts_member_call(
    fn_node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    scopes: &mut ScopeStack,
    in_value_pos: bool,
    skip_ids: &HashSet<usize>,
    root_fn_id: Option<usize>,
    refs: &mut RefCollector,
    depth: usize,
) {
    let content = ctx.content;
    let Some(prop) = fn_node.child_by_field_name("property") else {
        return;
    };
    let func_name = node_text(prop, content).trim();
    if let Some(obj) = fn_node.child_by_field_name("object") {
        let unwrapped_obj = unwrap_ts_expr(obj);
        let obj_text = node_text(unwrapped_obj, content).trim();
        let is_this = unwrapped_obj.kind() == "this" || obj_text == "this";
        if ctx.external_set.contains(obj_text) && is_valid_ident(func_name) {
            refs.push_call(&format!("{obj_text}.{func_name}"), false);
        } else if let Some(hint) = ctx.internal_namespaces.get(obj_text) {
            refs.push_call(&super::format_hinted_call(hint, func_name), false);
        } else if let Some(cls) = enclosing_class.filter(|_| is_this && !func_name.is_empty()) {
            refs.push_call(&format!("{cls}::{func_name}"), false);
        } else if let Some(ty) = scopes.get(obj_text) {
            refs.push_call(&format!("{ty}::{func_name}"), false);
        } else if is_ts_static_class_ident(obj_text) && is_valid_ident(func_name) {
            refs.push_call(&format!("{obj_text}::{func_name}"), false);
        } else if func_name != "this" && func_name != "super" {
            refs.push_call(func_name, true);
        }
        visit_ts_ref(
            obj,
            ctx,
            enclosing_class,
            scopes,
            in_value_pos,
            skip_ids,
            root_fn_id,
            refs,
            depth + 1,
        );
    } else if func_name != "this" && func_name != "super" {
        refs.push_call(func_name, true);
    }
}

fn extract_object_literal_members(
    object_node: Node,
    ctx: &TsWalkCtx<'_>,
    parent_name: &str,
    parent_exported: bool,
    symbols: &mut Vec<Symbol>,
    skip_ids: &mut HashSet<usize>,
) {
    let content = ctx.content;
    let mut member_flags = 0u8;
    if parent_exported {
        member_flags |= symbol_flags::IS_PUBLIC;
    }
    if ctx.file_is_test {
        member_flags |= symbol_flags::IS_TEST;
    }
    let mut cursor = object_node.walk();
    for child in object_node.children(&mut cursor) {
        if child.kind() == "pair" {
            if let Some(key_node) = child.child_by_field_name("key") {
                let key_name = node_text(key_node, content)
                    .trim_matches('\'')
                    .trim_matches('"');
                if !key_name.is_empty() {
                    let callable = child.child_by_field_name("value").is_some_and(|v| {
                        matches!(
                            unwrap_ts_expr(v).kind(),
                            "arrow_function"
                                | "function_expression"
                                | "function"
                                | "generator_function"
                        )
                    });
                    let full_name = format!("{parent_name}::{key_name}");
                    let kind = if callable {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Field
                    };
                    symbols.push(create_symbol(
                        child,
                        ctx,
                        full_name,
                        kind,
                        Some(parent_name),
                        member_flags,
                        &[],
                    ));
                    skip_ids.insert(child.id());
                }
            }
        } else if child.kind() == "method_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let key_name = node_text(name_node, content)
                    .trim_matches('\'')
                    .trim_matches('"');
                if !key_name.is_empty() {
                    let full_name = format!("{parent_name}::{key_name}");
                    symbols.push(create_symbol(
                        child,
                        ctx,
                        full_name,
                        SymbolKind::Method,
                        Some(parent_name),
                        member_flags,
                        &[],
                    ));
                    skip_ids.insert(child.id());
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
                    return grandparent.kind() == "export_statement"
                        || grandparent.kind() == "program";
                }
                return true;
            }
        }
    }
    false
}

fn extract_require_spec<'a>(val: Node, content: &'a str) -> Option<&'a str> {
    let u = unwrap_ts_expr(val);
    if u.kind() != "call_expression" {
        return None;
    }
    let fn_node = u.child_by_field_name("function")?;
    if node_text(fn_node, content).trim() != "require" {
        return None;
    }
    let args = u.child_by_field_name("arguments")?;
    let first = args.named_child(0)?;
    if first.kind() == "string" {
        return Some(node_text(first, content).trim_matches(['\'', '"', ' ']));
    }
    None
}

fn collect_external_imports(
    root: Node,
    content: &str,
    aliases: &AliasSet,
    depth_truncated: &Cell<bool>,
) -> Vec<String> {
    let mut out = ImportSet::default();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            let Some(source) = child.child_by_field_name("source") else {
                continue;
            };
            let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
            if spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec) {
                continue;
            }
            let mut ccursor = child.walk();
            for item in child.children(&mut ccursor) {
                collect_import_binding(item, content, &mut out, depth_truncated, 0);
            }
        } else if matches!(child.kind(), "lexical_declaration" | "variable_declaration") {
            let mut dcur = child.walk();
            for decl in child.children(&mut dcur) {
                if decl.kind() != "variable_declarator" {
                    continue;
                }
                let Some(spec) = decl
                    .child_by_field_name("value")
                    .and_then(|v| extract_require_spec(v, content))
                else {
                    continue;
                };
                if spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec) {
                    continue;
                }
                if let Some(name_node) = decl.child_by_field_name("name") {
                    let mut names = Vec::new();
                    extract_pattern_names(name_node, content, &mut names, depth_truncated, 0);
                    for n in names {
                        out.push(n);
                    }
                }
            }
        }
    }
    out.items
}

fn collect_local_imports(
    root: Node,
    content: &str,
    aliases: &AliasSet,
    depth_truncated: &Cell<bool>,
) -> (HashMap<String, (String, String)>, HashMap<String, String>) {
    let mut import_aliases = HashMap::new();
    let mut namespaces = HashMap::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            let Some(source) = child.child_by_field_name("source") else {
                continue;
            };
            let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
            let is_local = spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec);
            if !is_local {
                continue;
            }
            let hint = super::spec_to_module_hint(spec, aliases);
            collect_local_specifiers(
                child,
                content,
                &hint,
                &mut import_aliases,
                &mut namespaces,
                depth_truncated,
                0,
            );
        } else if matches!(child.kind(), "lexical_declaration" | "variable_declaration") {
            let mut dcur = child.walk();
            for decl in child.children(&mut dcur) {
                if decl.kind() != "variable_declarator" {
                    continue;
                }
                let Some(spec) = decl
                    .child_by_field_name("value")
                    .and_then(|v| extract_require_spec(v, content))
                else {
                    continue;
                };
                if !(spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec)) {
                    continue;
                }
                let hint = super::spec_to_module_hint(spec, aliases);
                if let Some(name_node) = decl.child_by_field_name("name") {
                    collect_require_local_bindings(
                        name_node,
                        content,
                        &hint,
                        &mut import_aliases,
                        &mut namespaces,
                    );
                }
            }
        }
    }
    (import_aliases, namespaces)
}

fn collect_require_local_bindings(
    name_node: Node,
    content: &str,
    hint: &str,
    import_aliases: &mut HashMap<String, (String, String)>,
    namespaces: &mut HashMap<String, String>,
) {
    match name_node.kind() {
        "identifier" => {
            let ns = node_text(name_node, content).trim();
            if is_valid_ident(ns) && !hint.is_empty() {
                namespaces.insert(ns.to_string(), hint.to_string());
            }
        }
        "object_pattern" => {
            let mut cur = name_node.walk();
            for item in name_node.named_children(&mut cur) {
                match item.kind() {
                    "shorthand_property_identifier_pattern" => {
                        let name = node_text(item, content).trim();
                        if is_valid_ident(name) && !hint.is_empty() {
                            import_aliases
                                .insert(name.to_string(), (hint.to_string(), name.to_string()));
                        }
                    }
                    "pair_pattern" => {
                        if let (Some(k), Some(v)) = (
                            item.child_by_field_name("key"),
                            item.child_by_field_name("value"),
                        ) {
                            let orig = node_text(k, content).trim();
                            let local = node_text(v, content).trim();
                            if is_valid_ident(orig) && is_valid_ident(local) {
                                import_aliases.insert(
                                    local.to_string(),
                                    (hint.to_string(), orig.to_string()),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn collect_local_specifiers(
    node: Node,
    content: &str,
    hint: &str,
    out: &mut HashMap<String, (String, String)>,
    namespaces: &mut HashMap<String, String>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "import_specifier" => {
            if let Some(orig_node) = node.child_by_field_name("name") {
                let orig = node_text(orig_node, content).trim();
                let alias = node
                    .child_by_field_name("alias")
                    .map(|a| node_text(a, content).trim())
                    .filter(|a| !a.is_empty());
                if !orig.is_empty() {
                    match alias {
                        Some(a) if a != orig => {
                            out.insert(a.to_string(), (hint.to_string(), orig.to_string()));
                        }
                        None if !hint.is_empty() => {
                            out.entry(orig.to_string())
                                .or_insert_with(|| (hint.to_string(), orig.to_string()));
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        "namespace_import" => {
            if let Some(id) = node.named_child(0) {
                let ns = node_text(id, content).trim();
                if is_valid_ident(ns) && !hint.is_empty() {
                    namespaces.insert(ns.to_string(), hint.to_string());
                }
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_local_specifiers(
            c,
            content,
            hint,
            out,
            namespaces,
            depth_truncated,
            depth + 1,
        );
    }
}

fn collect_import_binding(
    node: Node,
    content: &str,
    out: &mut ImportSet,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "default_import" | "identifier" => {
            out.push(node_text(node, content).trim());
        }
        "namespace_import" => {
            if let Some(id) = node.named_child(0) {
                out.push(node_text(id, content).trim());
            }
        }
        "import_specifier" => {
            let local = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"));
            if let Some(n) = local {
                out.push(node_text(n, content).trim());
            }
        }
        _ => {
            let mut cursor = node.walk();
            for c in node.children(&mut cursor) {
                collect_import_binding(c, content, out, depth_truncated, depth + 1);
            }
        }
    }
}

fn extract_pattern_names<'a>(
    node: Node,
    content: &'a str,
    out: &mut Vec<&'a str>,
    depth_truncated: &Cell<bool>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        depth_truncated.set(true);
        return;
    }
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            let s = node_text(node, content).trim();
            if !s.is_empty() {
                out.push(s);
            }
        }
        "pair_pattern" => {
            if let Some(val) = node.child_by_field_name("value") {
                extract_pattern_names(val, content, out, depth_truncated, depth + 1);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            let left = node
                .child_by_field_name("left")
                .or_else(|| node.named_child(0));
            if let Some(lhs) = left {
                extract_pattern_names(lhs, content, out, depth_truncated, depth + 1);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_pattern_names(child, content, out, depth_truncated, depth + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_aliases() -> AliasSet {
        AliasSet::default()
    }

    #[test]
    fn data_pairs_are_fields_not_methods() {
        let src =
            "export const r = makeRouter({\n  input: fileStream,\n  run: async () => {},\n});";
        let syms = parse_typescript("r.ts", src, false, &no_aliases()).unwrap();
        let kind_of = |n: &str| {
            syms.iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("no {n}"))
                .kind
                .clone()
        };
        assert_eq!(kind_of("r::input"), SymbolKind::Field);
        assert_eq!(kind_of("r::run"), SymbolKind::Method);
    }

    #[test]
    fn member_calls_are_flagged_bare_calls_are_not() {
        let src = "export function f(x: any) { x.input(); return bare(); }";
        let syms = parse_typescript("f.ts", src, false, &no_aliases()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert!(f.calls.contains(&"input".to_string()));
        assert!(f.calls.contains(&"bare".to_string()));
        assert!(f.member_calls.contains(&"input".to_string()));
        assert!(!f.member_calls.contains(&"bare".to_string()));
    }

    #[test]
    fn import_classification_is_three_way() {
        let src = concat!(
            "import { primaryKey } from \"drizzle-orm\";\n",
            "import { x } from \"./local\";\n",
            "import { aliased } from \"@app/server/services/permission\";\n",
            "export function f() { return primaryKey(); }"
        );
        let aliases = AliasSet::for_test(&["@app/server/*"]);
        let syms = parse_typescript("f.ts", src, false, &aliases).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        let imported = &f.external_imports;
        assert!(imported.contains(&"primaryKey".to_string()));
        assert!(!imported.contains(&"x".to_string()));
        assert!(!imported.contains(&"aliased".to_string()));
    }

    #[test]
    fn alias_binding_is_external_when_no_alias_is_declared() {
        let src = "import { aliased } from \"@app/server/services/permission\";\nexport function f() { return aliased(); }";
        let syms = parse_typescript("f.ts", src, false, &no_aliases()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert!(f.external_imports.contains(&"aliased".to_string()));
    }

    #[test]
    fn jsx_components_abstract_classes_and_destructured_signatures_work() {
        let src = r#"
            export abstract class Base { abstract run(): void; }
            export function Page({ title }: { title: string }) {
                const localTmp = compute();
                return <MyButton label={title} />;
            }
        "#;
        let syms = parse_typescript("Page.tsx", src, true, &no_aliases()).unwrap();
        assert!(syms
            .iter()
            .any(|s| s.name == "Base" && s.kind == SymbolKind::Class));
        assert!(!syms.iter().any(|s| s.name == "localTmp"));
        let page = syms.iter().find(|s| s.name == "Page").unwrap();
        assert!(
            page.signature.contains("{ title }"),
            "sig: {}",
            page.signature
        );
        assert!(page.mentions.contains(&"MyButton".to_string()));
        assert!(page.calls.contains(&"MyButton".to_string()));
    }

    #[test]
    fn ts_visibility_decorators_reexports_scopes_and_local_template_fix() {
        let src = r#"
            import * as ns from "./helpers";
            import { dup } from "./a";
            export { f } from "./b";

            @Controller()
            export class Svc {
                #secret() {}
                private priv() {}
                protected prot() {}
                @Get()
                pubMethod(x: Foo | null) {
                    const msg = `hi`;
                    [1].map((x: Bar) => x.b());
                    x.a();
                    Svc.staticFn();
                    ns.help();
                    dup();
                }
                static staticFn() {}
            }

            class Internal {
                hidden() {}
            }

            const localObj = { fn: () => {} };

            function postExported() {}
            export { postExported };
        "#;
        let syms = parse_typescript("svc.ts", src, false, &no_aliases()).unwrap();
        assert!(!syms.iter().any(|s| s.name == "msg"));
        assert!(syms.iter().any(|s| s.name == "f" && s.is_public()));

        let svc = syms.iter().find(|s| s.name == "Svc").unwrap();
        assert!(svc.is_public() && svc.is_attr_entry());
        let secret = syms.iter().find(|s| s.name == "Svc::#secret").unwrap();
        assert!(!secret.is_public());
        let priv_m = syms.iter().find(|s| s.name == "Svc::priv").unwrap();
        assert!(!priv_m.is_public());
        let prot_m = syms.iter().find(|s| s.name == "Svc::prot").unwrap();
        assert!(!prot_m.is_public());
        let hidden = syms.iter().find(|s| s.name == "Internal::hidden").unwrap();
        assert!(!hidden.is_public());
        let obj_fn = syms.iter().find(|s| s.name == "localObj::fn").unwrap();
        assert!(!obj_fn.is_public());
        let post = syms.iter().find(|s| s.name == "postExported").unwrap();
        assert!(post.is_public());

        let pm = syms.iter().find(|s| s.name == "Svc::pubMethod").unwrap();
        assert!(pm.is_public() && pm.is_attr_entry());
        assert!(pm.calls.contains(&"Foo::a".to_string()), "{:?}", pm.calls);
        assert!(!pm.calls.contains(&"Bar::a".to_string()), "{:?}", pm.calls);
        assert!(pm.calls.contains(&"Bar::b".to_string()), "{:?}", pm.calls);
        assert!(
            pm.calls.contains(&"Svc::staticFn".to_string()),
            "{:?}",
            pm.calls
        );
        assert!(
            pm.calls.contains(&"helpers:help".to_string()),
            "{:?}",
            pm.calls
        );
        assert!(pm.calls.contains(&"a:dup".to_string()), "{:?}", pm.calls);
        assert!(!pm.member_calls.contains(&"a".to_string()));
    }
}
