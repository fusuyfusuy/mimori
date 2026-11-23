use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub mod symbol_flags {
    pub const IS_TEST: u8 = 1 << 0;
    pub const IS_PUBLIC: u8 = 1 << 1;
    pub const IS_TRAIT_IMPL: u8 = 1 << 2;
    pub const IS_ATTR_ENTRY: u8 = 1 << 3;
}

mod arc_strs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(val: &Arc<[String]>, s: S) -> Result<S::Ok, S::Error> {
        val.as_ref().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[String]>, D::Error> {
        Vec::<String>::deserialize(d).map(Arc::from)
    }
}

pub fn empty_external_imports() -> Arc<[String]> {
    Arc::from([])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Interface,
    Trait,
    Enum,
    #[serde(rename = "type")]
    TypeAlias,
    Variable,
    Module,
    Constant,
    Field,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Field => "field",
            SymbolKind::Struct => "struct",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Enum => "enum",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Variable => "variable",
            SymbolKind::Module => "module",
            SymbolKind::Constant => "constant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default)]
    pub centrality: f64,
    /// True call edges (call/new expressions). Alone feeds PageRank/up/blast.
    #[serde(default)]
    pub calls: Vec<String>,
    /// Non-call mentions (member properties, type names, call-arg and
    /// template-interpolation identifiers). Backs `uses`, never call edges.
    #[serde(default)]
    pub mentions: Vec<String>,
    /// Per-call-name occurrence count (multiplicity before dedup).
    /// Weight = min(sqrt(n), 8). BTreeMap guarantees deterministic JSON order.
    #[serde(default)]
    pub call_counts: BTreeMap<String, u32>,
    /// Calls made through a receiver (`X.foo()`): weakest evidence, never
    /// leaves the file unless receiver type was statically inferred. Subset of `calls`.
    #[serde(default)]
    pub member_calls: Vec<String>,
    /// Local names bound by this file to external (non-relative) modules.
    /// Calls matching one are external — never resolved locally.
    #[serde(default = "empty_external_imports", with = "arc_strs")]
    pub external_imports: Arc<[String]>,
    /// AST-derived semantic flags (`symbol_flags::IS_TEST`, `IS_PUBLIC`,
    /// `IS_TRAIT_IMPL`, `IS_ATTR_ENTRY`).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub flags: u8,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

impl Symbol {
    pub fn coordinate(&self) -> String {
        format!("{}:{}", self.file, self.name)
    }

    pub fn line_coordinate(&self) -> String {
        format!("{}:#L{}-{}", self.file, self.start_line, self.end_line)
    }

    pub fn is_test_flag(&self) -> bool {
        (self.flags & symbol_flags::IS_TEST) != 0
    }

    pub fn is_public(&self) -> bool {
        (self.flags & symbol_flags::IS_PUBLIC) != 0
    }

    pub fn is_trait_impl(&self) -> bool {
        (self.flags & symbol_flags::IS_TRAIT_IMPL) != 0
    }

    pub fn is_attr_entry(&self) -> bool {
        (self.flags & symbol_flags::IS_ATTR_ENTRY) != 0
    }
}
