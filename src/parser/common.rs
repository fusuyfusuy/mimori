use crate::model::{Symbol, SymbolKind};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node;

/// Hard AST recursion bound shared across all polyglot parsers to prevent stack
/// overflow on pathological or adversarial syntax trees.
pub const MAX_AST_DEPTH: usize = 128;

/// Structured parse health diagnostics surfaced alongside extracted symbols.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseDiagnostics {
    pub has_error: bool,
    pub depth_truncated: bool,
}

impl ParseDiagnostics {
    pub fn has_issues(&self) -> bool {
        self.has_error || self.depth_truncated
    }
}

/// Lexical scope stack tracking local variable -> inferred receiver type mappings.
/// Storing `Option<String>` ensures an untyped inner re-binding (`let x = unknown()`)
/// shadows an outer typed binding instead of leaking the outer type.
#[derive(Default)]
pub struct ScopeStack {
    frames: Vec<HashMap<String, Option<String>>>,
}

impl ScopeStack {
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
        }
    }

    pub fn push(&mut self) {
        self.frames.push(HashMap::new());
    }

    pub fn pop(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    pub fn insert(&mut self, name: String, ty: String) {
        if let Some(top) = self.frames.last_mut() {
            top.insert(name, Some(ty));
        }
    }

    pub fn shadow(&mut self, name: String) {
        if let Some(top) = self.frames.last_mut() {
            top.insert(name, None);
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        for frame in self.frames.iter().rev() {
            if let Some(entry) = frame.get(name) {
                return entry.as_deref();
            }
        }
        None
    }
}

/// Slice raw source bytes for a Tree-sitter node safely.
pub fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    content
        .get(node.start_byte()..node.end_byte())
        .unwrap_or("")
}

/// Deduplicated ordered list of imported identifier names.
#[derive(Default)]
pub struct ImportSet {
    pub items: Vec<String>,
    seen: HashSet<String>,
}

impl ImportSet {
    pub fn push(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && name != "*" && self.seen.insert(name.to_string()) {
            self.items.push(name.to_string());
        }
    }
}

/// Shared reference accumulator for calls, mentions, deterministic call counts
/// (`BTreeMap`), and O(1) member-vs-bare tracking.
#[derive(Default)]
pub struct RefCollector {
    pub calls: Vec<String>,
    pub mentions: Vec<String>,
    mention_set: HashSet<String>,
    pub counts: BTreeMap<String, u32>,
    pub member_calls: Vec<String>,
    member_set: HashSet<String>,
    bare_calls: HashSet<String>,
}

impl RefCollector {
    pub fn push_call(&mut self, name: &str, is_member: bool) {
        let name = name.trim();
        if name.is_empty() {
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
            if !self.bare_calls.contains(name) && self.member_set.insert(name.to_string()) {
                self.member_calls.push(name.to_string());
            }
        } else if self.bare_calls.insert(name.to_string()) && self.member_set.remove(name) {
            self.member_calls.retain(|c| c != name);
        }
    }

    pub fn push_mention(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && self.mention_set.insert(name.to_string()) {
            self.mentions.push(name.to_string());
        }
    }
}

/// Shared constructor for extracted `Symbol` records across all language parsers.
pub fn build_symbol(
    node: Node,
    file: &str,
    name: String,
    kind: SymbolKind,
    signature: String,
    refs: RefCollector,
    flags: u8,
) -> Symbol {
    let start_pos = node.start_position();
    let end_pos = node.end_position();
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

/// Extract clean single-line signature using an AST `body` child boundary when
/// available so braces inside destructuring params, generics, or string
/// literals never truncate signatures prematurely.
pub fn extract_signature_ast(node: Node, content: &str, fallback_first_line: &str) -> String {
    if let Some(body_node) = node.child_by_field_name("body") {
        if body_node.start_byte() >= node.start_byte() {
            if let Some(sig_raw) = content.get(node.start_byte()..body_node.start_byte()) {
                let sig = sig_raw
                    .trim()
                    .trim_end_matches(':')
                    .trim()
                    .replace('\n', " ");
                if !sig.is_empty() {
                    return sig;
                }
            }
        }
    }
    fallback_first_line.to_string()
}

/// True when `s` is a valid simple identifier (not a chained expression like
/// `"get_factory()"` or `"arr[0]"`).
pub fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}
