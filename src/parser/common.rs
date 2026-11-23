use std::collections::{BTreeMap, HashSet};
use tree_sitter::Node;

/// Hard AST recursion bound shared across all polyglot parsers to prevent stack
/// overflow on pathological or adversarial syntax trees.
pub const MAX_AST_DEPTH: usize = 128;

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
