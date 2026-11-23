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
    static PY_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("Error loading Python grammar");
        p
    });
}

struct PyImports {
    /// Local module binding -> module hint for workspace-internal `import a [as b]`.
    internal_modules: HashMap<String, String>,
    /// Local bindings originating from stdlib or third-party external packages.
    external: HashSet<String>,
    external_list: Vec<String>,
}

struct PyWalkCtx<'a> {
    content: &'a str,
    file: &'a str,
    file_is_test: bool,
    imports: &'a PyImports,
}

pub fn parse_python(file: &str, content: &str, aliases: &AliasSet) -> Result<Vec<Symbol>> {
    let tree = PY_PARSER
        .with(|p| p.borrow_mut().parse(content, None))
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;

    let root = tree.root_node();
    let imports = collect_py_imports(root, content, aliases);
    let file_is_test = file.starts_with("tests/")
        || file.contains("/tests/")
        || file
            .rsplit('/')
            .next()
            .is_some_and(|b| b.starts_with("test_") || b.ends_with("_test.py"));

    let ctx = PyWalkCtx {
        content,
        file,
        file_is_test,
        imports: &imports,
    };

    let mut symbols = Vec::new();
    walk_python_node(root, &ctx, None, &mut symbols, None, 0);

    let mut import_aliases = HashMap::new();
    collect_py_local_aliases(root, content, aliases, &mut import_aliases, 0);
    super::apply_import_aliases(&mut symbols, &import_aliases);

    let external_imports: Arc<[String]> = imports.external_list.into();
    for s in &mut symbols {
        s.external_imports = Arc::clone(&external_imports);
    }

    Ok(symbols)
}

fn is_module_level_assignment(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "module"
        || (parent.kind() == "expression_statement"
            && parent.parent().is_some_and(|gp| gp.kind() == "module"))
}

fn py_flags(name: &str, file_is_test: bool) -> u8 {
    let mut flags = 0u8;
    let leaf = name.rsplit("::").next().unwrap_or(name);
    if file_is_test || leaf.starts_with("test_") {
        flags |= symbol_flags::IS_TEST;
    }
    if !leaf.starts_with('_') {
        flags |= symbol_flags::IS_PUBLIC;
    }
    flags
}

fn walk_python_node(
    node: Node,
    ctx: &PyWalkCtx<'_>,
    parent_class: Option<&str>,
    symbols: &mut Vec<Symbol>,
    outer_node: Option<Node>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let content = ctx.content;
    let kind = node.kind();
    match kind {
        "function_definition" | "async_function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
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
                    ctx,
                    full_name.clone(),
                    sym_kind,
                    parent_class,
                    py_flags(&full_name, ctx.file_is_test),
                );
                symbols.push(symbol);
            }
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim();
                let symbol_node = outer_node.unwrap_or(node);
                let symbol = create_symbol(
                    symbol_node,
                    ctx,
                    name.to_string(),
                    SymbolKind::Class,
                    Some(name),
                    py_flags(name, ctx.file_is_test),
                );
                symbols.push(symbol);

                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_python_node(child, ctx, Some(name), symbols, None, depth + 1);
                    }
                }
                return;
            }
        }
        "decorated_definition" => {
            if let Some(def_node) = node.child_by_field_name("definition") {
                walk_python_node(def_node, ctx, parent_class, symbols, Some(node), depth + 1);
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
                    let name = node_text(left, content).trim();
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
                    let symbol = create_symbol(
                        node,
                        ctx,
                        name.to_string(),
                        sym_kind,
                        None,
                        py_flags(name, ctx.file_is_test),
                    );
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
        walk_python_node(child, ctx, next_parent, symbols, None, depth + 1);
    }
}

fn collect_py_local_types(node: Node, content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let def_node = if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    };
    if let Some(params) = def_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for p in params.children(&mut cursor) {
            if matches!(p.kind(), "typed_parameter" | "typed_default_parameter") {
                let var = p
                    .named_child(0)
                    .map(|n| node_text(n, content).trim())
                    .unwrap_or("");
                let ty = p
                    .child_by_field_name("type")
                    .map(|t| node_text(t, content).trim())
                    .unwrap_or("");
                let simple = ty.rsplit('.').next().unwrap_or(ty).trim();
                if is_valid_ident(var)
                    && is_valid_ident(simple)
                    && simple.starts_with(|c: char| c.is_ascii_uppercase())
                {
                    map.insert(var.to_string(), simple.to_string());
                }
            }
        }
    }
    map
}

fn create_symbol(
    node: Node,
    ctx: &PyWalkCtx<'_>,
    name: String,
    kind: SymbolKind,
    enclosing_class: Option<&str>,
    flags: u8,
) -> Symbol {
    let content = ctx.content;
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    let raw_body = node_text(node, content);
    let first_line = raw_body.lines().next().unwrap_or("").trim();
    let def_node = if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    };
    let signature = extract_signature_ast(def_node, content, first_line);

    let local_types = if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        collect_py_local_types(node, content)
    } else {
        HashMap::new()
    };

    let mut refs = RefCollector::default();
    collect_references(node, ctx, enclosing_class, &local_types, &mut refs, 0);

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

fn collect_references(
    node: Node,
    ctx: &PyWalkCtx<'_>,
    enclosing_class: Option<&str>,
    local_types: &HashMap<String, String>,
    refs: &mut RefCollector,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let content = ctx.content;
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
            extract_py_call(child, ctx, enclosing_class, local_types, refs);
        } else if kind == "attribute" {
            if let Some(attr) = child.child_by_field_name("attribute") {
                let a = node_text(attr, content).trim();
                if a != "self" && a != "cls" {
                    refs.push_mention(a);
                }
            }
        } else if kind == "type" {
            collect_type_mentions(child, content, refs, depth + 1);
            continue;
        }
        collect_references(child, ctx, enclosing_class, local_types, refs, depth + 1);
    }
}

fn extract_py_call(
    call_node: Node,
    ctx: &PyWalkCtx<'_>,
    enclosing_class: Option<&str>,
    local_types: &HashMap<String, String>,
    refs: &mut RefCollector,
) {
    let content = ctx.content;
    if let Some(func_node) = call_node.child_by_field_name("function") {
        if func_node.kind() == "attribute" {
            let func_name = func_node
                .child_by_field_name("attribute")
                .map(|a| node_text(a, content).trim())
                .unwrap_or("");
            if is_valid_ident(func_name) && func_name != "self" && func_name != "cls" {
                if let Some(obj) = func_node.child_by_field_name("object") {
                    let recv = node_text(obj, content).trim();
                    if ctx.imports.external.contains(recv) {
                        refs.push_call(&format!("{recv}.{func_name}"), false);
                    } else if let Some(hint) = ctx.imports.internal_modules.get(recv) {
                        refs.push_call(&super::format_hinted_call(hint, func_name), false);
                    } else {
                        refs.push_call(func_name, true);
                        if (recv == "self" || recv == "cls") && enclosing_class.is_some() {
                            if let Some(cls_name) = enclosing_class {
                                refs.push_call(&format!("{cls_name}::{func_name}"), false);
                            }
                        } else if let Some(ty) = local_types.get(recv) {
                            refs.push_call(&format!("{ty}::{func_name}"), false);
                        }
                    }
                } else {
                    refs.push_call(func_name, true);
                }
            }
        } else if func_node.kind() == "identifier" {
            let func_name = node_text(func_node, content).trim();
            if is_valid_ident(func_name) && func_name != "self" && func_name != "cls" {
                refs.push_call(func_name, false);
            }
        }
    }
    if let Some(args_node) = call_node.child_by_field_name("arguments") {
        let mut acursor = args_node.walk();
        for arg in args_node.children(&mut acursor) {
            if arg.kind() == "identifier" {
                let t = node_text(arg, content).trim();
                if t != "self" && t != "cls" {
                    refs.push_mention(t);
                }
            } else if arg.kind() == "attribute" {
                if let Some(attr) = arg.child_by_field_name("attribute") {
                    refs.push_mention(node_text(attr, content));
                }
            } else if arg.kind() == "keyword_argument" {
                if let Some(val) = arg.child_by_field_name("value") {
                    if val.kind() == "identifier" {
                        let t = node_text(val, content).trim();
                        if t != "self" && t != "cls" {
                            refs.push_mention(t);
                        }
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

fn is_external_py_module(module: &str, aliases: &AliasSet) -> bool {
    if module.starts_with('.') {
        return false;
    }
    let top = module.split('.').next().unwrap_or(module).trim();
    if top.is_empty() {
        return false;
    }
    if PYTHON_STDLIB.contains(&top) {
        return true;
    }
    // When workspace Python modules were discovered, any non-workspace top-level
    // import (e.g. `requests`, `pydantic`, `numpy`) is a third-party external package.
    aliases.has_python_modules() && !aliases.matches_python(top)
}

fn collect_py_local_aliases(
    node: Node,
    content: &str,
    aliases: &AliasSet,
    out: &mut HashMap<String, (String, String)>,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    if node.kind() == "import_from_statement" {
        if let Some(parsed) = parse_from_import_ast(node, content) {
            if !is_external_py_module(parsed.module, aliases) {
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
        collect_py_local_aliases(c, content, aliases, out, depth + 1);
    }
}

fn collect_py_imports(root: Node, content: &str, aliases: &AliasSet) -> PyImports {
    let mut internal_modules = HashMap::new();
    let mut ext_set = ImportSet::default();
    walk_py_imports(
        root,
        content,
        aliases,
        &mut internal_modules,
        &mut ext_set,
        0,
    );
    let external: HashSet<String> = ext_set.items.iter().cloned().collect();
    PyImports {
        internal_modules,
        external,
        external_list: ext_set.items,
    }
}

fn walk_py_imports(
    node: Node,
    content: &str,
    aliases: &AliasSet,
    internal_modules: &mut HashMap<String, String>,
    out: &mut ImportSet,
    depth: usize,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();
    if kind == "import_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "aliased_import" => {
                    if let Some(orig_node) = child.child_by_field_name("name") {
                        let orig = node_text(orig_node, content).trim();
                        let top = orig.split('.').next().unwrap_or(orig).trim();
                        let bound = child
                            .child_by_field_name("alias")
                            .map(|a| node_text(a, content).trim())
                            .unwrap_or(top);
                        if is_external_py_module(orig, aliases) {
                            out.push(bound);
                        } else {
                            internal_modules.insert(bound.to_string(), orig.replace('.', "/"));
                        }
                    }
                }
                "dotted_name" => {
                    let orig = node_text(child, content).trim();
                    let top = orig.split('.').next().unwrap_or(orig).trim();
                    if is_external_py_module(orig, aliases) {
                        out.push(top);
                    } else {
                        internal_modules.insert(top.to_string(), orig.replace('.', "/"));
                    }
                }
                _ => {}
            }
        }
        return;
    }
    if kind == "import_from_statement" {
        if let Some(parsed) = parse_from_import_ast(node, content) {
            if is_external_py_module(parsed.module, aliases) {
                for (orig, alias) in parsed.items {
                    if let Some(alias) = alias {
                        out.push(alias);
                    } else {
                        out.push(orig.split('.').next().unwrap_or(orig));
                    }
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        walk_py_imports(c, content, aliases, internal_modules, out, depth + 1);
    }
}

const PYTHON_STDLIB: &[&str] = &[
    "__future__",
    "abc",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "calendar",
    "cgi",
    "cgitb",
    "cmd",
    "code",
    "codecs",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "cProfile",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "numbers",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "sys",
    "sysconfig",
    "syslog",
    "tarfile",
    "tempfile",
    "termios",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "wsgiref",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_mentions_collected() {
        let src = r#"
def process(item: Item) -> Result:
    x = item.value
    return handle(arg_one, key=arg_two)
"#;
        let syms = parse_python("app.py", src, &AliasSet::default()).unwrap();
        let p = syms.iter().find(|s| s.name == "process").unwrap();
        assert!(p.mentions.contains(&"Item".to_string()));
        assert!(p.mentions.contains(&"Result".to_string()));
        assert!(p.mentions.contains(&"value".to_string()));
        assert!(p.mentions.contains(&"arg_one".to_string()));
        assert!(p.mentions.contains(&"arg_two".to_string()));
    }

    #[test]
    fn uuid_stdlib_and_relative_dot_alias_behave_correctly() {
        let src = r#"
from uuid import uuid4
from . import helper as h
def run():
    uuid4()
    h()
    get_factory()()
"#;
        let syms = parse_python("app.py", src, &AliasSet::default()).unwrap();
        let run = syms.iter().find(|s| s.name == "run").unwrap();
        assert!(run.external_imports.contains(&"uuid4".to_string()));
        assert!(run.calls.contains(&"helper".to_string()), "{:?}", run.calls);
        assert!(!run.calls.contains(&":helper".to_string()));
        assert!(!run.calls.iter().any(|c| c.contains("()")));
    }
}
