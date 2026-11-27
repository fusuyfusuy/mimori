use anyhow::{bail, Context, Result};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// File extensions that make a `:`/`#`-prefix recognizable as a path rather than
/// the first segment of a qualified symbol name.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go",
];

/// A parsed target coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coordinate {
    /// `path/file.rs:#L10-50`
    Lines {
        file: PathBuf,
        start: usize,
        end: usize,
    },
    /// `path/file.rs:symbol` or `path/file.rs#symbol`
    Symbol { file: PathBuf, name: String },
    /// `symbol` or `Type::method`
    Bare { name: String },
}

impl Coordinate {
    pub fn parse(raw: &str) -> Result<Self> {
        let mut raw = raw.trim();
        if let Some(idx) = raw.rfind(" (L") {
            let inner = &raw[idx + 2..];
            if let Some(body) = inner.strip_suffix(')') {
                if looks_like_line_range_token(body) {
                    raw = raw[..idx].trim();
                }
            }
        }
        if raw.is_empty() {
            bail!("Empty coordinate.");
        }

        // Case-insensitive `:#L` / `:#l` line range (works for any file extension, e.g. Cargo.toml:#L1-2).
        if let Some(idx) = find_colon_hash_line(raw) {
            let file = &raw[..idx];
            if file.is_empty() {
                bail!("Line coordinate '{}' has no file part.", raw);
            }
            let (start, end) = parse_line_range(&raw[idx + 3..])?;
            return Ok(Coordinate::Lines {
                file: PathBuf::from(file),
                start,
                end,
            });
        }

        // Split on a path/symbol ':' separator (handling Windows drive letters and POSIX colon paths).
        if let Some(idx) = find_path_colon(raw) {
            let (head, tail) = (&raw[..idx], &raw[idx + 1..]);
            return parse_colon_tail(raw, head, tail);
        }

        // Split on the last '#' to support `file.rs#Symbol`, `file.rs#L10-20`, and `Cargo.toml#L1-2`,
        // while tolerating directories containing '#' (e.g. `dir#1/b.rs#foo`).
        if let Some(idx) = raw.rfind('#') {
            let (head, tail) = (&raw[..idx], &raw[idx + 1..]);
            if tail.is_empty() && looks_like_any_file(head) {
                bail!("Coordinate '{}' has an empty '#' anchor.", raw);
            }
            let looks_like_line_anchor =
                looks_like_line_range_token(tail) || looks_like_digits_range(tail);
            if looks_like_line_anchor && looks_like_any_file(head) {
                return parse_hash_line(raw, head, tail);
            }
            if looks_like_path(head) && !tail.is_empty() {
                return Ok(Coordinate::Symbol {
                    file: PathBuf::from(head),
                    name: tail.to_string(),
                });
            }
        }

        Ok(Coordinate::Bare {
            name: raw.to_string(),
        })
    }

    /// Rewrite the file part into the workspace-relative form the index stores,
    /// so an absolute target and a relative one resolve identically.
    pub fn normalize_against(self, root: &Path) -> Self {
        match self {
            lines @ Coordinate::Lines { .. } => lines,
            Coordinate::Symbol { file, name } => Coordinate::Symbol {
                file: relativize(&file, root),
                name,
            },
            bare => bare,
        }
    }

    pub fn file(&self) -> Option<&Path> {
        match self {
            Coordinate::Lines { file, .. } | Coordinate::Symbol { file, .. } => Some(file),
            Coordinate::Bare { .. } => None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Coordinate::Symbol { name, .. } | Coordinate::Bare { name } => Some(name),
            Coordinate::Lines { .. } => None,
        }
    }

    pub fn absolute_parent(&self) -> Option<PathBuf> {
        let file = self.file()?;
        if !file.is_absolute() {
            return None;
        }
        file.parent().map(|p| p.to_path_buf())
    }
}

impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Coordinate::Lines { file, start, end } => {
                write!(f, "{}:#L{}-{}", file.display(), start, end)
            }
            Coordinate::Symbol { file, name } => write!(f, "{}:{}", file.display(), name),
            Coordinate::Bare { name } => write!(f, "{}", name),
        }
    }
}

fn find_colon_hash_line(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    for i in (0..bytes.len().saturating_sub(2)).rev() {
        if bytes[i] == b':' && bytes[i + 1] == b'#' && matches!(bytes[i + 2], b'L' | b'l') {
            return Some(i);
        }
    }
    None
}

fn find_path_colon(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    let start = if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        2
    } else {
        0
    };

    let mut fallback: Option<usize> = None;
    let mut best_file_split: Option<usize> = None;

    for i in start..bytes.len() {
        if bytes[i] != b':' {
            continue;
        }
        // Skip `::` qualified symbol separators.
        let prev_colon = i > start && bytes[i - 1] == b':';
        let next_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
        if prev_colon || next_colon {
            continue;
        }
        let head = &raw[..i];
        let tail = &raw[i + 1..];
        let tail_is_line_spec = tail.starts_with('#') || looks_like_line_range_token(tail);
        if !(looks_like_path(head) || (tail_is_line_spec && looks_like_any_file(head))) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(i);
        }
        let head_has_ext = Path::new(head).extension().is_some();
        let tail_has_sep = tail.contains('/') || tail.contains('\\');
        if head_has_ext && !tail_has_sep {
            best_file_split = Some(i);
        }
    }

    best_file_split.or(fallback)
}

fn looks_like_digits_range(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    match s.split_once('-') {
        Some((a, b)) => {
            let b = b.strip_prefix(['L', 'l']).unwrap_or(b);
            !a.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && !b.is_empty()
                && b.bytes().all(|c| c.is_ascii_digit())
        }
        None => s.bytes().all(|c| c.is_ascii_digit()),
    }
}

fn looks_like_line_range_token(s: &str) -> bool {
    let Some(rest) = s.strip_prefix(['L', 'l']) else {
        return false;
    };
    looks_like_digits_range(rest)
}

fn parse_colon_tail(raw: &str, head: &str, tail: &str) -> Result<Coordinate> {
    if tail.is_empty() {
        bail!("Coordinate '{}' names a file but no symbol.", raw);
    }
    if let Some(hash_tail) = tail.strip_prefix('#') {
        return parse_hash_line(raw, head, hash_tail);
    }
    if looks_like_line_range_token(tail) {
        let rest = tail.strip_prefix(['L', 'l']).unwrap_or(tail);
        let (start, end) = parse_line_range(rest)?;
        return Ok(Coordinate::Lines {
            file: PathBuf::from(head),
            start,
            end,
        });
    }
    Ok(Coordinate::Symbol {
        file: PathBuf::from(head),
        name: tail.to_string(),
    })
}

fn parse_hash_line(raw: &str, head: &str, tail: &str) -> Result<Coordinate> {
    if head.is_empty() {
        bail!("Line coordinate '{}' has no file part.", raw);
    }
    let rest = tail.strip_prefix(['L', 'l']).unwrap_or(tail);
    if rest.is_empty() {
        bail!("Line coordinate '{}' has an empty line range.", raw);
    }
    let (start, end) = parse_line_range(rest)?;
    Ok(Coordinate::Lines {
        file: PathBuf::from(head),
        start,
        end,
    })
}

fn parse_line_range(range: &str) -> Result<(usize, usize)> {
    let (start, end) = match range.split_once('-') {
        Some((s, e)) => {
            let e = e.trim();
            let e = e.strip_prefix(['L', 'l']).unwrap_or(e);
            (
                s.trim().parse::<usize>().context("Invalid start line")?,
                e.trim().parse::<usize>().context("Invalid end line")?,
            )
        }
        None => {
            let n: usize = range.trim().parse().context("Invalid line number")?;
            (n, n)
        }
    };
    if start == 0 || end == 0 {
        bail!("Line numbers must be >= 1 (got '{}').", range);
    }
    Ok((start, end))
}

fn looks_like_path(head: &str) -> bool {
    if head.contains('/') || head.contains('\\') {
        return true;
    }
    Path::new(head)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

fn looks_like_any_file(head: &str) -> bool {
    if head.is_empty() {
        return false;
    }
    head.contains('/') || head.contains('\\') || Path::new(head).extension().is_some()
}

/// Strip the workspace root and any `./` noise, leaving the form stored in the
/// index.
fn relativize(file: &Path, root: &Path) -> PathBuf {
    let stripped = file.strip_prefix(root).unwrap_or(file);
    let mut parts: Vec<Component<'_>> = Vec::new();
    for comp in stripped.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(comp);
                }
            }
            other => parts.push(other),
        }
    }
    let cleaned: PathBuf = parts.into_iter().collect();
    if cleaned.as_os_str().is_empty() {
        stripped.to_path_buf()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_are_not_mistaken_for_paths() {
        assert_eq!(
            Coordinate::parse("Store::save").unwrap(),
            Coordinate::Bare {
                name: "Store::save".into()
            }
        );
        assert_eq!(
            Coordinate::parse("handler").unwrap(),
            Coordinate::Bare {
                name: "handler".into()
            }
        );
    }

    #[test]
    fn paths_are_recognized_by_separator_or_extension() {
        assert_eq!(
            Coordinate::parse("src/a.rs:handler").unwrap(),
            Coordinate::Symbol {
                file: "src/a.rs".into(),
                name: "handler".into()
            }
        );
        assert_eq!(
            Coordinate::parse("a.rs:handler").unwrap(),
            Coordinate::Symbol {
                file: "a.rs".into(),
                name: "handler".into()
            }
        );
    }

    #[test]
    fn hash_symbol_and_special_paths_parse_cleanly() {
        assert_eq!(
            Coordinate::parse("src/lib.rs#Phase").unwrap(),
            Coordinate::Symbol {
                file: "src/lib.rs".into(),
                name: "Phase".into()
            }
        );
        assert_eq!(
            Coordinate::parse("dir#1/b.rs:target").unwrap(),
            Coordinate::Symbol {
                file: "dir#1/b.rs".into(),
                name: "target".into()
            }
        );
        assert_eq!(
            Coordinate::parse("Cargo.toml#L1-2").unwrap(),
            Coordinate::Lines {
                file: "Cargo.toml".into(),
                start: 1,
                end: 2
            }
        );
        assert_eq!(
            Coordinate::parse("Cargo.toml:#l1-2").unwrap(),
            Coordinate::Lines {
                file: "Cargo.toml".into(),
                start: 1,
                end: 2
            }
        );
        assert_eq!(
            Coordinate::parse("a.rs:L5").unwrap(),
            Coordinate::Lines {
                file: "a.rs".into(),
                start: 5,
                end: 5
            }
        );
        assert_eq!(
            Coordinate::parse("a.rs:foo (Linux)").unwrap(),
            Coordinate::Symbol {
                file: "a.rs".into(),
                name: "foo (Linux)".into()
            }
        );
        assert_eq!(
            Coordinate::parse("a.rs:foo (L10-L20)").unwrap(),
            Coordinate::Symbol {
                file: "a.rs".into(),
                name: "foo".into()
            }
        );
    }

    #[test]
    fn a_qualified_name_survives_a_file_prefix() {
        assert_eq!(
            Coordinate::parse("src/a.rs:Store::save").unwrap(),
            Coordinate::Symbol {
                file: "src/a.rs".into(),
                name: "Store::save".into()
            }
        );
    }

    #[test]
    fn line_ranges_parse_in_both_forms() {
        assert_eq!(
            Coordinate::parse("a.rs:#L10-50").unwrap(),
            Coordinate::Lines {
                file: "a.rs".into(),
                start: 10,
                end: 50
            }
        );
        assert_eq!(
            Coordinate::parse("a.rs:#L7").unwrap(),
            Coordinate::Lines {
                file: "a.rs".into(),
                start: 7,
                end: 7
            }
        );
    }

    #[test]
    fn absolute_targets_normalize_onto_stored_paths() {
        let root = Path::new("/repo");
        let c = Coordinate::parse("/repo/src/a.rs:handler")
            .unwrap()
            .normalize_against(root);
        assert_eq!(c.file().unwrap(), Path::new("src/a.rs"));

        let c = Coordinate::parse("./src/a.rs:handler")
            .unwrap()
            .normalize_against(root);
        assert_eq!(c.file().unwrap(), Path::new("src/a.rs"));
    }

    #[test]
    fn malformed_coordinates_are_errors_not_guesses() {
        assert!(Coordinate::parse("").is_err());
        assert!(Coordinate::parse("a.rs:").is_err());
        assert!(Coordinate::parse("a.rs:#Lxx").is_err());
        assert!(Coordinate::parse("a.rs:#L0").is_err());
        assert!(Coordinate::parse("a.rs:#L0-0").is_err());
        assert!(Coordinate::parse("a.rs#L0-5").is_err());
    }

    #[test]
    fn l_digit_identifiers_and_colon_paths_parse_cleanly() {
        assert_eq!(
            Coordinate::parse("math.py:l2_norm").unwrap(),
            Coordinate::Symbol {
                file: "math.py".into(),
                name: "l2_norm".into()
            }
        );
        assert_eq!(
            Coordinate::parse("cache.rs:L1Cache").unwrap(),
            Coordinate::Symbol {
                file: "cache.rs".into(),
                name: "L1Cache".into()
            }
        );
        assert_eq!(
            Coordinate::parse("proxy.rs:L4Proxy").unwrap(),
            Coordinate::Symbol {
                file: "proxy.rs".into(),
                name: "L4Proxy".into()
            }
        );
        assert_eq!(
            Coordinate::parse("cache.rs#L1Cache").unwrap(),
            Coordinate::Symbol {
                file: "cache.rs".into(),
                name: "L1Cache".into()
            }
        );
        assert_eq!(
            Coordinate::parse("dir:v1/a.rs:foo").unwrap(),
            Coordinate::Symbol {
                file: "dir:v1/a.rs".into(),
                name: "foo".into()
            }
        );
        assert_eq!(
            Coordinate::parse("a/b:v1/c.rs:Store::save").unwrap(),
            Coordinate::Symbol {
                file: "a/b:v1/c.rs".into(),
                name: "Store::save".into()
            }
        );
        assert_eq!(
            Coordinate::parse("dir:v1/Cargo.toml:L1-5").unwrap(),
            Coordinate::Lines {
                file: "dir:v1/Cargo.toml".into(),
                start: 1,
                end: 5
            }
        );
        assert_eq!(
            Coordinate::parse(r"C:\src\a.rs:Store::save").unwrap(),
            Coordinate::Symbol {
                file: r"C:\src\a.rs".into(),
                name: "Store::save".into()
            }
        );
    }
}
