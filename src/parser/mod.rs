pub mod go;
pub mod lang;
pub mod python;
pub mod rust;
pub mod typescript;

pub use lang::Language;

use crate::model::Symbol;
use anyhow::{bail, Result};
use std::path::Path;

pub fn parse_file(path: &Path, content: &str) -> Result<Vec<Symbol>> {
    let lang = Language::from_path(path);
    let file_str = path.to_str().unwrap_or("");

    match lang {
        Language::Rust => rust::parse_rust(file_str, content),
        Language::TypeScript | Language::JavaScript => {
            typescript::parse_typescript(file_str, content, false)
        }
        Language::TSX => typescript::parse_typescript(file_str, content, true),
        Language::Python => python::parse_python(file_str, content),
        Language::Go => go::parse_go(file_str, content),
        Language::Unknown => bail!("Unsupported language for file: {}", path.display()),
    }
}
