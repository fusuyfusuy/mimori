use crate::model::{Symbol, SymbolKind};
use crate::workspace::AliasSet;
use anyhow::Result;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

    let mut symbols = Vec::new();
    let root = tree.root_node();
    walk_ts_node(root, content, file, None, aliases, &mut symbols, 0);
    let import_aliases = collect_local_import_aliases(root, content, aliases);
    super::apply_import_aliases(&mut symbols, &import_aliases);
    // File-level facts stamp every symbol identically; threading them
    // through the walker would churn a dozen call sites for no gain.
    let external_imports: Arc<[String]> = collect_external_imports(root, content, aliases).into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

const MAX_AST_DEPTH: usize = 512;

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

#[allow(clippy::too_many_arguments)]
fn walk_ts_node(
    node: Node,
    content: &str,
    file: &str,
    parent_class: Option<&str>,
    aliases: &AliasSet,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    match kind {
        "export_statement" if node.child_by_field_name("source").is_some() => {
            extract_reexport_symbols(node, content, file, aliases, symbols);
            return;
        }
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Function,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
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
                        walk_ts_node(
                            child,
                            content,
                            file,
                            Some(name),
                            aliases,
                            symbols,
                            depth + 1,
                        );
                    }
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Interface,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::TypeAlias,
                    None,
                );
                symbols.push(symbol);
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    name.to_string(),
                    SymbolKind::Enum,
                    None,
                );
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
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    full_name,
                    SymbolKind::Method,
                    parent_class,
                );
                symbols.push(symbol);
                // P1: `constructor(private cmd: ...)` declares a field.
                // Tree-sitter marks these as required/optional parameters
                // carrying an accessibility modifier.
                if name == "constructor" {
                    if let Some(pc) = parent_class {
                        extract_ctor_param_fields(node, content, file, pc, symbols);
                    }
                }
            }
        }
        "variable_declarator" => {
            if handle_variable_declarator(node, content, file, symbols) {
                return;
            }
        }
        _ if kind.ends_with("field_definition") => {
            // P1: `command: string;` / `readonly x = 1` inside a class body.
            // SEXP: (public_field_definition name: (property_identifier)).
            if let Some(pc) = parent_class {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let fname = node_text(name_node, content).trim();
                    if !fname.is_empty() {
                        let full_name = format!("{}::{}", pc, fname);
                        let symbol = create_symbol(
                            node,
                            content,
                            file,
                            full_name,
                            SymbolKind::Field,
                            Some(pc),
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
        walk_ts_node(
            child,
            content,
            file,
            next_parent,
            aliases,
            symbols,
            depth + 1,
        );
    }
}

fn handle_variable_declarator(
    node: Node,
    content: &str,
    file: &str,
    symbols: &mut Vec<Symbol>,
) -> bool {
    let (Some(name_node), Some(raw_val_node)) = (
        node.child_by_field_name("name"),
        node.child_by_field_name("value"),
    ) else {
        return false;
    };

    if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
        if is_top_level_declarator(node) {
            let mut names = Vec::new();
            extract_pattern_names(name_node, content, &mut names, 0);
            for pname in names {
                let symbol = create_symbol(
                    node,
                    content,
                    file,
                    pname.to_string(),
                    SymbolKind::Variable,
                    None,
                );
                symbols.push(symbol);
            }
        }
        return true;
    }

    let name = node_text(name_node, content);
    let value_node = unwrap_ts_expr(raw_val_node);
    let val_kind = value_node.kind();
    if matches!(
        val_kind,
        "arrow_function" | "function" | "function_expression" | "generator_function"
    ) {
        let symbol = create_symbol(
            node,
            content,
            file,
            name.to_string(),
            SymbolKind::Function,
            None,
        );
        symbols.push(symbol);
    } else if val_kind == "object" && is_top_level_declarator(node) {
        let sym_kind = if is_top_level_const(node, content) {
            SymbolKind::Constant
        } else {
            SymbolKind::Variable
        };
        let mut members = Vec::new();
        let mut skip_ids = Vec::new();
        extract_object_literal_members(
            value_node,
            content,
            file,
            name,
            &mut members,
            &mut skip_ids,
        );
        let symbol = create_symbol_skipping(
            node,
            content,
            file,
            name.to_string(),
            sym_kind,
            None,
            &skip_ids,
        );
        symbols.push(symbol);
        symbols.extend(members);
        return true;
    } else if val_kind == "call_expression" {
        if let Some(args_node) = value_node.child_by_field_name("arguments") {
            let mut members = Vec::new();
            let mut skip_ids = Vec::new();
            let mut arg_cursor = args_node.walk();
            for arg in args_node.children(&mut arg_cursor) {
                let unwrapped_arg = unwrap_ts_expr(arg);
                if unwrapped_arg.kind() == "object" {
                    extract_object_literal_members(
                        unwrapped_arg,
                        content,
                        file,
                        name,
                        &mut members,
                        &mut skip_ids,
                    );
                }
            }
            let symbol = create_symbol_skipping(
                node,
                content,
                file,
                name.to_string(),
                SymbolKind::Variable,
                None,
                &skip_ids,
            );
            symbols.push(symbol);
            symbols.extend(members);
            // Return as the class_declaration arm does. Falling
            // through let the generic recursion revisit the same
            // method_definition nodes with parent_class = None,
            // emitting every shorthand method a second time.
            return true;
        }
        let symbol = create_symbol(
            node,
            content,
            file,
            name.to_string(),
            SymbolKind::Variable,
            None,
        );
        symbols.push(symbol);
    } else if val_kind == "template_string" {
        // P1b: `const commandWithLog = `(${cmd}) >> ...`` is a
        // coarse dataflow node: its template mentions flow into
        // it, and call args carrying it flow out to exec sinks.
        let symbol = create_symbol(
            node,
            content,
            file,
            name.to_string(),
            SymbolKind::Variable,
            None,
        );
        symbols.push(symbol);
    } else if is_top_level_const(node, content) {
        let symbol = create_symbol(
            node,
            content,
            file,
            name.to_string(),
            SymbolKind::Constant,
            None,
        );
        symbols.push(symbol);
    }
    false
}

fn extract_reexport_symbols(
    node: Node,
    content: &str,
    file: &str,
    aliases: &AliasSet,
    symbols: &mut Vec<Symbol>,
) {
    let Some(source) = node.child_by_field_name("source") else {
        return;
    };
    let spec = node_text(source, content).trim_matches(['\'', '"', ' ']);
    let is_local = spec.starts_with('.') || spec.starts_with('/') || aliases.matches(spec);
    if !is_local {
        return;
    }
    let hint = super::spec_to_module_hint(spec);
    let stmt_is_type = node_text(node, content)
        .trim_start()
        .starts_with("export type ");
    collect_reexport_specifiers(node, content, file, &hint, stmt_is_type, symbols, 0);
}

fn collect_reexport_specifiers(
    node: Node,
    content: &str,
    file: &str,
    hint: &str,
    stmt_is_type: bool,
    symbols: &mut Vec<Symbol>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
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
                let mut call_counts = HashMap::new();
                if is_type {
                    mentions.push(orig.to_string());
                } else {
                    let target = if hint.is_empty() {
                        orig.to_string()
                    } else {
                        format!("{hint}:{orig}")
                    };
                    call_counts.insert(target.clone(), 1);
                    calls.push(target);
                }
                symbols.push(Symbol {
                    name: alias.to_string(),
                    kind,
                    file: file.to_string(),
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
                });
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        collect_reexport_specifiers(c, content, file, hint, stmt_is_type, symbols, depth + 1);
    }
}

/// Index `constructor(private cmd: string)` parameter properties as fields.
fn extract_ctor_param_fields(
    ctor_node: Node,
    content: &str,
    file: &str,
    parent_class: &str,
    symbols: &mut Vec<Symbol>,
) {
    let Some(params) = ctor_node.child_by_field_name("parameters") else {
        return;
    };
    let mut cursor = params.walk();
    for param in params.children(&mut cursor) {
        let pk = param.kind();
        if pk != "required_parameter" && pk != "optional_parameter" {
            continue;
        }
        // A parameter property carries an explicit accessibility modifier
        // (`private`/`public`/`protected`/`readonly`). Plain parameters do not
        // become fields and must not be indexed.
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
                    content,
                    file,
                    full_name,
                    SymbolKind::Field,
                    Some(parent_class),
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
        || (parent.kind() == "lexical_declaration"
            && parent
                .parent()
                .is_some_and(|gp| gp.kind() == "export_statement"))
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
        if name.is_empty() || name == "this" || name == "super" {
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
        if name.is_empty() || name == "this" || name == "super" {
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
    create_symbol_skipping(node, content, file, name, kind, enclosing_class, &[])
}

fn create_symbol_skipping(
    node: Node,
    content: &str,
    file: &str,
    name: String,
    kind: SymbolKind,
    enclosing_class: Option<&str>,
    skip_ids: &[usize],
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let raw_sig = extract_signature(raw_body, &kind);
    let signature = if is_exported_ts_node(node) && !raw_sig.starts_with("export ") {
        format!("export {raw_sig}")
    } else {
        raw_sig
    };

    let mut refs = RefCollector::default();
    collect_references(
        node,
        content,
        enclosing_class,
        false,
        skip_ids,
        &mut refs,
        0,
    );

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
            content,
            enclosing_class,
            in_value_pos,
            skip_ids,
            refs,
            depth + 1,
        );
    }
}

fn visit_ts_ref(
    node: Node,
    content: &str,
    enclosing_class: Option<&str>,
    in_value_pos: bool,
    skip_ids: &[usize],
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH || skip_ids.contains(&node.id()) {
        return;
    }
    let kind = node.kind();
    if matches!(
        kind,
        "method_definition"
            | "class_declaration"
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
                content,
                enclosing_class,
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
                if !short.is_empty() && short != "super" {
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
                    content,
                    enclosing_class,
                    child_val,
                    skip_ids,
                    refs,
                    depth + 1,
                );
            }
        }
        "member_expression" => {
            if let Some(prop) = node.child_by_field_name("property") {
                refs.push_mention(node_text(prop, content).trim());
            }
            if let Some(obj) = node.child_by_field_name("object") {
                visit_ts_ref(
                    obj,
                    content,
                    enclosing_class,
                    in_value_pos,
                    skip_ids,
                    refs,
                    depth + 1,
                );
            }
        }
        "template_substitution" => {
            collect_references(node, content, enclosing_class, true, skip_ids, refs, depth);
        }
        "type_identifier" => {
            refs.push_mention(node_text(node, content).trim());
        }
        "identifier" => {
            if in_value_pos {
                refs.push_mention(node_text(node, content).trim());
            }
        }
        _ => {
            collect_references(
                node,
                content,
                enclosing_class,
                in_value_pos,
                skip_ids,
                refs,
                depth,
            );
        }
    }
}

fn visit_ts_call(
    node: Node,
    content: &str,
    enclosing_class: Option<&str>,
    in_value_pos: bool,
    skip_ids: &[usize],
    refs: &mut RefCollector,
    depth: usize,
) {
    let func_node = node.child_by_field_name("function");
    if let Some(raw_fn) = func_node {
        let fn_node = unwrap_ts_expr(raw_fn);
        if fn_node.kind() == "member_expression" {
            if let Some(prop) = fn_node.child_by_field_name("property") {
                let func_name = node_text(prop, content).trim();
                refs.push_call(func_name, true);
                if let Some(obj) = fn_node.child_by_field_name("object") {
                    let unwrapped_obj = unwrap_ts_expr(obj);
                    let is_this = unwrapped_obj.kind() == "this"
                        || node_text(unwrapped_obj, content).trim() == "this";
                    if let Some(cls) = enclosing_class.filter(|_| is_this && !func_name.is_empty())
                    {
                        refs.push_call(&format!("{cls}::{func_name}"), false);
                    }
                    visit_ts_ref(
                        obj,
                        content,
                        enclosing_class,
                        in_value_pos,
                        skip_ids,
                        refs,
                        depth + 1,
                    );
                }
            }
        } else if fn_node.kind() == "identifier" {
            refs.push_call(node_text(fn_node, content).trim(), false);
        } else {
            visit_ts_ref(
                fn_node,
                content,
                enclosing_class,
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
            content,
            enclosing_class,
            child_val,
            skip_ids,
            refs,
            depth + 1,
        );
    }
}

fn extract_object_literal_members(
    object_node: Node,
    content: &str,
    file: &str,
    parent_name: &str,
    symbols: &mut Vec<Symbol>,
    skip_ids: &mut Vec<usize>,
) {
    let mut cursor = object_node.walk();
    for child in object_node.children(&mut cursor) {
        if child.kind() == "pair" {
            if let Some(key_node) = child.child_by_field_name("key") {
                let key_name = node_text(key_node, content)
                    .trim_matches('\'')
                    .trim_matches('"');
                if !key_name.is_empty() {
                    // `input: fileStream` is data, not a callable: only a
                    // function-valued property keeps Method kind, so
                    // non-callables can never be call targets.
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
                    let symbol =
                        create_symbol(child, content, file, full_name, kind, Some(parent_name));
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
                    let symbol = create_symbol(
                        child,
                        content,
                        file,
                        full_name,
                        SymbolKind::Method,
                        Some(parent_name),
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

/// Local names bound to modules outside this workspace in this file.
/// `import { primaryKey } from "drizzle-orm"` binds `primaryKey`;
/// `import x from "./local"` binds nothing (resolvable locally), and neither
/// does `import { y } from "@app/server/y"` when `@app/*` is a declared
/// `compilerOptions.paths` alias -- an alias is this workspace's own code.
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
        if spec.starts_with('.') || spec.starts_with('/') {
            continue;
        }
        if aliases.matches(spec) {
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
        if !matches!(child.kind(), "import_statement" | "export_statement") {
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
    if matches!(node.kind(), "import_specifier" | "export_specifier") {
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
        "import_specifier" => {
            // `import { a as b }`: the local binding is the alias.
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

    /// No aliases configured: relative and package importers only.
    fn no_aliases() -> AliasSet {
        AliasSet::default()
    }

    #[test]
    fn data_pairs_are_fields_not_methods() {
        // `input: fileStream` is data — only function-valued properties
        // keep Method kind, so non-callables are never call targets.
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
        assert!(
            f.calls.contains(&"input".to_string()),
            "calls: {:?}",
            f.calls
        );
        assert!(
            f.calls.contains(&"bare".to_string()),
            "calls: {:?}",
            f.calls
        );
        assert!(f.member_calls.contains(&"input".to_string()));
        assert!(!f.member_calls.contains(&"bare".to_string()));
    }

    /// Three-way, not two-way: relative, path alias, package. Ensures
    /// monorepo path aliases are correctly classified as first-party.
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
        assert!(
            imported.contains(&"primaryKey".to_string()),
            "package binding must stay external: {imported:?}"
        );
        assert!(
            !imported.contains(&"x".to_string()),
            "relative binding must stay internal: {imported:?}"
        );
        assert!(
            !imported.contains(&"aliased".to_string()),
            "path-alias binding must be internal: {imported:?}"
        );
    }

    /// Without the alias declaration the same import is a package again —
    /// the gate is driven by tsconfig, not by the specifier's shape.
    #[test]
    fn alias_binding_is_external_when_no_alias_is_declared() {
        let src = "import { aliased } from \"@app/server/services/permission\";\nexport function f() { return aliased(); }";
        let syms = parse_typescript("f.ts", src, false, &no_aliases()).unwrap();
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert!(f.external_imports.contains(&"aliased".to_string()));
    }
}
