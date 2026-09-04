use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Interface,
    Trait,
    Enum,
    TypeAlias,
    Variable,
    Module,
    Constant,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
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
    pub body: String,
    pub doc: Option<String>,
    #[serde(default)]
    pub centrality: f64,
    #[serde(default)]
    pub references: Vec<String>,
}

impl Symbol {
    pub fn coordinate(&self) -> String {
        format!("{}:{}", self.file, self.name)
    }

    pub fn line_coordinate(&self) -> String {
        format!("{}:#L{}-{}", self.file, self.start_line, self.end_line)
    }
}
