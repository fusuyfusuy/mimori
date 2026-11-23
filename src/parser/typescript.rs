use crate::model::{symbol_flags, Symbol, SymbolKind};
use crate::parser::common::{
    extract_signature_ast, is_valid_ident, node_text, ImportSet, RefCollector, MAX_AST_DEPTH,
};
use crate::workspace::AliasSet;
use anyhow::Result;
use std::cell::RefCell;
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
}

pub fn parse_typescript(
    file: &str,
    content: &str,
    is_tsx: bool,
    aliases: &AliasSet,
) -> Result<Vec<Symbol>> {
    let tree = if is_tsx {
        TSX_PARSER.with(|p| p.borrow_mut().parse(content, None))
    } else {
        TS_PARSER.with(|p| p.borrow_mut().parse(content, None))
    }
    .ok_or_else(|| anyhow::anyhow!("Failed to parse TypeScript source"))?;

    let root = tree.root_node();
    let external_list = collect_external_imports(root, content, aliases);
    let external_set: HashSet<String> = external_list.iter().cloned().collect();
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
    };

    let mut symbols = Vec::new();
    walk_ts_node(root, &ctx, None, &mut symbols, 0);

    let import_aliases = collect_local_import_aliases(root, content, aliases);
    super::apply_import_aliases(&mut symbols, &import_aliases);

    let external_imports: Arc<[String]> = external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
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

fn ts_flags(node: Node, file_is_test: bool, is_class_member: bool, content: &str) -> u8 {
    let mut flags = 0u8;
    if file_is_test {
        flags |= symbol_flags::IS_TEST;
    }
    if is_exported_ts_node(node) {
        flags |= symbol_flags::IS_PUBLIC;
    } else if is_class_member {
        let mut is_priv = false;
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            if c.kind() == "accessibility_modifier" {
                let m = node_text(c, content).trim();
                if m == "private" {
                    is_priv = true;
                }
            }
        }
        if !is_priv {
            flags |= symbol_flags::IS_PUBLIC;
        }
    }
    flags
}

fn walk_ts_node(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    parent_class: Option<&str>,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let content = ctx.content;
    let kind = node.kind();
    match kind {
        "export_statement" if node.child_by_field_name("source").is_some() => {
            extract_reexport_symbols(node, ctx, symbols);
            return;
        }
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                    ts_flags(node, ctx.file_is_test, false, content),
                );
                symbols.push(symbol);
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Class,
                    Some(name),
                    ts_flags(node, ctx.file_is_test, false, content),
                );
                symbols.push(symbol);

                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_ts_node(child, ctx, Some(name), symbols, depth + 1);
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Interface,
                    None,
                    ts_flags(node, ctx.file_is_test, false, content),
                );
                symbols.push(symbol);
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::TypeAlias,
                    None,
                    ts_flags(node, ctx.file_is_test, false, content),
                );
                symbols.push(symbol);
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol = create_symbol(
                    node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Enum,
                    None,
                    ts_flags(node, ctx.file_is_test, false, content),
                );
                symbols.push(symbol);
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let full_name = match parent_class {
                    Some(pc) => format!("{}::{}", pc, name),
                    None => name.to_string(),
                };
                let symbol = create_symbol(
                    node,
                    ctx,
                    full_name,
                    SymbolKind::Method,
                    parent_class,
                    ts_flags(node, ctx.file_is_test, parent_class.is_some(), content),
                );
                symbols.push(symbol);
                if name == "constructor" {
                    if let Some(pc) = parent_class {
                        extract_ctor_param_fields(node, ctx, pc, symbols);
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
                        let full_name = format!("{}::{}", pc, fname);
                        let symbol = create_symbol(
                            node,
                            ctx,
                            full_name,
                            SymbolKind::Field,
                            Some(pc),
                            ts_flags(node, ctx.file_is_test, true, content),
                        );
                        symbols.push(symbol);
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ts_node(child, ctx, next_parent, symbols, depth + 1);
    }
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
    let flags = ts_flags(node, ctx.file_is_test, false, content);

    if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
        if top_level {
            let mut names = Vec::new();
            extract_pattern_names(name_node, content, &mut names, 0);
            for pname in names {
                let symbol = create_symbol(
                    node,
                    ctx,
                    pname.to_string(),
                    SymbolKind::Variable,
                    None,
                    flags,
                );
                symbols.push(symbol);
            }
        }
        return true;
    }

    let name = node_text(name_node, content).trim();
    let value_node = unwrap_ts_expr(raw_val_node);
    let val_kind = value_node.kind();

    if matches!(
        val_kind,
        "arrow_function" | "function" | "function_expression" | "generator_function"
    ) && top_level
    {
        let symbol = create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Function,
            None,
            flags,
        );
        symbols.push(symbol);
    } else if val_kind == "object" && top_level {
        let sym_kind = if is_top_level_const(node, content) {
            SymbolKind::Constant
        } else {
            SymbolKind::Variable
        };
        let mut members = Vec::new();
        let mut skip_ids = Vec::new();
        extract_object_literal_members(value_node, ctx, name, &mut members, &mut skip_ids);
        let symbol = create_symbol_skipping(
            node,
            ctx,
            name.to_string(),
            sym_kind,
            None,
            flags,
            &skip_ids,
        );
        symbols.push(symbol);
        symbols.extend(members);
        return true;
    } else if val_kind == "call_expression" && top_level {
        if let Some(args_node) = value_node.child_by_field_name("arguments") {
            let mut members = Vec::new();
            let mut skip_ids = Vec::new();
            let mut arg_cursor = args_node.walk();
            for arg in args_node.children(&mut arg_cursor) {
                let unwrapped_arg = unwrap_ts_expr(arg);
                if unwrapped_arg.kind() == "object" {
                    extract_object_literal_members(
                        unwrapped_arg,
                        ctx,
                        name,
                        &mut members,
                        &mut skip_ids,
                    );
                }
            }
            let symbol = create_symbol_skipping(
                node,
                ctx,
                name.to_string(),
                SymbolKind::Variable,
                None,
                flags,
                &skip_ids,
            );
            symbols.push(symbol);
            symbols.extend(members);
            return true;
        }
        let symbol = create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Variable,
            None,
            flags,
        );
        symbols.push(symbol);
    } else if val_kind == "template_string" {
        // Preserved coarse dataflow node for template interpolation tracking (`up`/`uses`).
        let symbol = create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Variable,
            None,
            flags,
        );
        symbols.push(symbol);
    } else if is_top_level_const(node, content) {
        let symbol = create_symbol(
            node,
            ctx,
            name.to_string(),
            SymbolKind::Constant,
            None,
            flags,
        );
        symbols.push(symbol);
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
    let hint = super::spec_to_module_hint(spec);
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
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let content = ctx.content;
    if node.kind() == "export_specifier" {
        if let (Some(orig_node), Some(alias_node)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("alias"),
        ) {
            let orig = node_text(orig_node, content).trim();
            let alias = node_text(alias_node, content).trim();
            if !orig.is_empty() && !alias.is_empty() && orig != alias {
                let is_type =
                    stmt_is_type || node_text(node, content).trim_start().starts_with("type ");
                let kind = if is_type {
                    SymbolKind::TypeAlias
                } else {
                    SymbolKind::Function
                };
                let start_pos = node.start_position();
                let end_pos = node.end_position();
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
                symbols.push(Symbol {
                    name: alias.to_string(),
                    kind,
                    file: ctx.file.to_string(),
                    start_line: start_pos.row + 1,
                    end_line: end_pos.row + 1,
                    signature: format!("export {{ {orig} as {alias} }}"),
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
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_reexport_specifiers(c, ctx, hint, stmt_is_type, symbols, depth + 1);
    }
}

fn extract_ctor_param_fields(
    ctor_node: Node,
    ctx: &TsWalkCtx<'_>,
    parent_class: &str,
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
                let full_name = format!("{}::{}", parent_class, fname);
                let symbol = create_symbol(
                    param,
                    ctx,
                    full_name,
                    SymbolKind::Field,
                    Some(parent_class),
                    ts_flags(param, ctx.file_is_test, true, content),
                );
                symbols.push(symbol);
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

fn collect_ts_local_types(node: Node, content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    collect_ts_typed_bindings(node, content, &mut map, 0);
    map
}

fn collect_ts_typed_bindings(
    node: Node,
    content: &str,
    out: &mut HashMap<String, String>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    match node.kind() {
        "required_parameter" | "optional_parameter" => {
            if let (Some(pat), Some(ty_ann)) = (
                node.child_by_field_name("pattern"),
                node.child_by_field_name("type"),
            ) {
                let var = node_text(pat, content).trim();
                if let Some(ty) = extract_ts_type_ident(ty_ann, content) {
                    if is_valid_ident(var) {
                        out.insert(var.to_string(), ty);
                    }
                }
            }
        }
        "variable_declarator" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let var = node_text(name_node, content).trim();
                if is_valid_ident(var) {
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| extract_ts_type_ident(t, content))
                        .or_else(|| {
                            node.child_by_field_name("value").and_then(|v| {
                                let u = unwrap_ts_expr(v);
                                if u.kind() == "new_expression" {
                                    u.child_by_field_name("constructor").and_then(|c| {
                                        let t = node_text(c, content)
                                            .rsplit('.')
                                            .next()
                                            .unwrap_or("")
                                            .trim();
                                        (is_valid_ident(t)
                                            && t.starts_with(|ch: char| ch.is_ascii_uppercase()))
                                        .then(|| t.to_string())
                                    })
                                } else {
                                    None
                                }
                            })
                        });
                    if let Some(ty_name) = ty {
                        out.insert(var.to_string(), ty_name);
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if !matches!(
            c.kind(),
            "class_declaration"
                | "abstract_class_declaration"
                | "function_declaration"
                | "method_definition"
        ) {
            collect_ts_typed_bindings(c, content, out, depth + 1);
        }
    }
}

fn extract_ts_type_ident(node: Node, content: &str) -> Option<String> {
    let mut cur = node;
    for _ in 0..6 {
        match cur.kind() {
            "type_annotation" => cur = cur.named_child(0)?,
            "generic_type" => cur = cur.child_by_field_name("name")?,
            "type_identifier" => {
                let t = node_text(cur, content).trim();
                return (is_valid_ident(t)
                    && t.starts_with(|c: char| c.is_ascii_uppercase())
                    && !matches!(t, "Array" | "Promise" | "Record" | "Map" | "Set" | "Error"))
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
) -> Symbol {
    create_symbol_skipping(node, ctx, name, kind, enclosing_class, flags, &[])
}

fn create_symbol_skipping(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    enclosing_class: Option<&str>,
    flags: u8,
    skip_ids: &[usize],
) -> Symbol {
    let content = ctx.content;
    let start_pos = node.start_position();
    let end_pos = node.end_position();
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

    let local_types = if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        collect_ts_local_types(node, content)
    } else {
        HashMap::new()
    };

    let mut refs = RefCollector::default();
    collect_references(
        node,
        ctx,
        enclosing_class,
        &local_types,
        false,
        skip_ids,
        &mut refs,
        0,
    );

    Symbol {
        name,
        kind,
        file: ctx.file.to_string(),
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

#[allow(clippy::too_many_arguments)]
fn collect_references(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    local_types: &HashMap<String, String>,
    in_value_pos: bool,
    skip_ids: &[usize],
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_ts_ref(
            child,
            ctx,
            enclosing_class,
            local_types,
            in_value_pos,
            skip_ids,
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
    local_types: &HashMap<String, String>,
    in_value_pos: bool,
    skip_ids: &[usize],
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH || skip_ids.contains(&node.id()) {
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
        "call_expression" => {
            visit_ts_call(
                node,
                ctx,
                enclosing_class,
                local_types,
                in_value_pos,
                skip_ids,
                refs,
                depth,
            );
        }
        "new_expression" => {
            let ctor_node = node.child_by_field_name("constructor");
            if let Some(ctor) = ctor_node {
                let text = node_text(ctor, content);
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
                    local_types,
                    child_val,
                    skip_ids,
                    refs,
                    depth + 1,
                );
            }
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
                local_types,
                in_value_pos,
                skip_ids,
                refs,
                depth,
            );
        }
        "jsx_expression" => {
            collect_references(
                node,
                ctx,
                enclosing_class,
                local_types,
                true,
                skip_ids,
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
                    local_types,
                    in_value_pos,
                    skip_ids,
                    refs,
                    depth + 1,
                );
            }
        }
        "template_substitution" => {
            collect_references(
                node,
                ctx,
                enclosing_class,
                local_types,
                true,
                skip_ids,
                refs,
                depth,
            );
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
                local_types,
                in_value_pos,
                skip_ids,
                refs,
                depth,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_ts_call(
    node: Node,
    ctx: &TsWalkCtx<'_>,
    enclosing_class: Option<&str>,
    local_types: &HashMap<String, String>,
    in_value_pos: bool,
    skip_ids: &[usize],
    refs: &mut RefCollector,
    depth: usize,
) {
    let content = ctx.content;
    let func_node = node.child_by_field_name("function");
    if let Some(raw_fn) = func_node {
        let fn_node = unwrap_ts_expr(raw_fn);
        if fn_node.kind() == "member_expression" {
            if let Some(prop) = fn_node.child_by_field_name("property") {
                let func_name = node_text(prop, content).trim();
                if let Some(obj) = fn_node.child_by_field_name("object") {
                    let unwrapped_obj = unwrap_ts_expr(obj);
                    let obj_text = node_text(unwrapped_obj, content).trim();
                    if ctx.external_set.contains(obj_text) && is_valid_ident(func_name) {
                        refs.push_call(&format!("{obj_text}.{func_name}"), false);
                    } else {
                        if func_name != "this" && func_name != "super" {
                            refs.push_call(func_name, true);
                        }
                        let is_this = unwrapped_obj.kind() == "this" || obj_text == "this";
                        if let Some(cls) =
                            enclosing_class.filter(|_| is_this && !func_name.is_empty())
                        {
                            refs.push_call(&format!("{cls}::{func_name}"), false);
                        } else if let Some(ty) = local_types.get(obj_text) {
                            refs.push_call(&format!("{ty}::{func_name}"), false);
                        }
                    }
                    visit_ts_ref(
                        obj,
                        ctx,
                        enclosing_class,
                        local_types,
                        in_value_pos,
                        skip_ids,
                        refs,
                        depth + 1,
                    );
                } else if func_name != "this" && func_name != "super" {
                    refs.push_call(func_name, true);
                }
            }
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
                local_types,
                in_value_pos,
                skip_ids,
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
            local_types,
            child_val,
            skip_ids,
            refs,
            depth + 1,
        );
    }
}

fn extract_object_literal_members(
    object_node: Node,
    ctx: &TsWalkCtx<'_>,
    parent_name: &str,
    symbols: &mut Vec<Symbol>,
    skip_ids: &mut Vec<usize>,
) {
    let content = ctx.content;
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
                    let full_name = format!("{}::{}", parent_name, key_name);
                    let kind = if callable {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Field
                    };
                    let mut flags = symbol_flags::IS_PUBLIC;
                    if ctx.file_is_test {
                        flags |= symbol_flags::IS_TEST;
                    }
                    let symbol =
                        create_symbol(child, ctx, full_name, kind, Some(parent_name), flags);
                    skip_ids.push(child.id());
                    symbols.push(symbol);
                }
            }
        } else if child.kind() == "method_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let key_name = node_text(name_node, content)
                    .trim_matches('\'')
                    .trim_matches('"');
                if !key_name.is_empty() {
                    let full_name = format!("{}::{}", parent_name, key_name);
                    let mut flags = symbol_flags::IS_PUBLIC;
                    if ctx.file_is_test {
                        flags |= symbol_flags::IS_TEST;
                    }
                    let symbol = create_symbol(
                        child,
                        ctx,
                        full_name,
                        SymbolKind::Method,
                        Some(parent_name),
                        flags,
                    );
                    skip_ids.push(child.id());
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
                    return grandparent.kind() == "export_statement"
                        || grandparent.kind() == "program";
                }
                return true;
            }
        }
    }
    false
}

fn collect_external_imports(root: Node, content: &str, aliases: &AliasSet) -> Vec<String> {
    let mut out = ImportSet::default();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() != "import_statement" {
            continue;
        }
        let Some(source) = child.child_by_field_name("source") else {
            continue;
        };
        let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
        if spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec) {
            continue;
        }
        let mut ccursor = child.walk();
        for item in child.children(&mut ccursor) {
            collect_import_binding(item, content, &mut out, 0);
        }
    }
    out.items
}

fn collect_local_import_aliases(
    root: Node,
    content: &str,
    aliases: &AliasSet,
) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        // Strictly `import_statement` — re-exports (`export { a as b } from ...`) do not bind local names.
        if child.kind() != "import_statement" {
            continue;
        }
        let Some(source) = child.child_by_field_name("source") else {
            continue;
        };
        let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
        let is_local = spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec);
        if !is_local {
            continue;
        }
        let hint = super::spec_to_module_hint(spec);
        collect_aliased_specifiers(child, content, &hint, &mut out, 0);
    }
    out
}

fn collect_aliased_specifiers(
    node: Node,
    content: &str,
    hint: &str,
    out: &mut HashMap<String, (String, String)>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_specifier" {
        if let (Some(orig_node), Some(alias_node)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("alias"),
        ) {
            let orig = node_text(orig_node, content).trim();
            let alias = node_text(alias_node, content).trim();
            if !orig.is_empty() && !alias.is_empty() && orig != alias {
                out.insert(alias.to_string(), (hint.to_string(), orig.to_string()));
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_aliased_specifiers(c, content, hint, out, depth + 1);
    }
}

fn collect_import_binding(node: Node, content: &str, out: &mut ImportSet, depth: usize) {
    if depth >= MAX_AST_DEPTH {
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
                collect_import_binding(c, content, out, depth + 1);
            }
        }
    }
}

fn extract_pattern_names<'a>(node: Node, content: &'a str, out: &mut Vec<&'a str>, depth: usize) {
    if depth >= MAX_AST_DEPTH {
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
                extract_pattern_names(val, content, out, depth + 1);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            let left = node
                .child_by_field_name("left")
                .or_else(|| node.named_child(0));
            if let Some(lhs) = left {
                extract_pattern_names(lhs, content, out, depth + 1);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_pattern_names(child, content, out, depth + 1);
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
}
