use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Transaction};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

/// Bumped whenever parsing or the stored symbol/reference format changes.
///
/// v15: AST `SymbolFlags` (`flags` column), normalized `files.external_imports_json`,
/// dead `idx_symbols_name` removed, atomic `BEGIN IMMEDIATE` swap+load.
pub const PARSER_VERSION: i64 = 15;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open_or_create(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )?;

        let mut db = Database { conn };
        db.ensure_schema_and_version()?;
        Ok(db)
    }

    /// Open the SQLite cache, automatically unlinking and rebuilding once if
    /// the ephemeral `.mimori/index.db` file is corrupt (`SQLITE_NOTADB` / `SQLITE_CORRUPT`).
    pub fn open_or_rebuild(db_path: &Path) -> Result<Self> {
        match Self::open_or_create(db_path) {
            Ok(db) => Ok(db),
            Err(err) if is_corruption_error(&err) => {
                unlink_db_artifacts(db_path);
                Self::open_or_create(db_path)
            }
            Err(err) => Err(err),
        }
    }

    fn ensure_schema_and_version(&mut self) -> Result<()> {
        if self.schema_is_current()? {
            return Ok(());
        }

        let _ = self.conn.execute_batch("PRAGMA journal_mode = WAL;");
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Re-check inside BEGIN IMMEDIATE to prevent two cold opens from racing.
        if schema_is_current_tx(&tx)? {
            tx.commit()?;
            return Ok(());
        }
        tx.execute_batch(
            "DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS files;
             DROP TABLE IF EXISTS meta;

             CREATE TABLE files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT UNIQUE NOT NULL,
                 mtime INTEGER NOT NULL,
                 hash TEXT NOT NULL,
                 external_imports_json TEXT NOT NULL DEFAULT '[]'
             );

             CREATE TABLE symbols (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 file_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 signature TEXT NOT NULL,
                 references_json TEXT NOT NULL DEFAULT '[]',
                 mentions_json TEXT NOT NULL DEFAULT '[]',
                 call_counts_json TEXT NOT NULL DEFAULT '{}',
                 member_calls_json TEXT NOT NULL DEFAULT '[]',
                 flags INTEGER NOT NULL DEFAULT 0,
                 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
             );

             CREATE INDEX idx_symbols_file_id ON symbols(file_id);

             CREATE TABLE meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        tx.execute_batch(&format!("PRAGMA user_version = {PARSER_VERSION};"))?;
        tx.commit()?;
        Ok(())
    }

    fn schema_is_current(&self) -> Result<bool> {
        let found: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found != PARSER_VERSION {
            return Ok(false);
        }
        let table_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('files', 'symbols', 'meta')",
            [],
            |row| row.get(0),
        )?;
        Ok(table_count == 3)
    }

    pub fn alias_fingerprint(&self) -> Result<Option<String>> {
        let stored = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'alias_fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored)
    }

    pub fn get_file_records(&self) -> Result<HashMap<String, (i64, i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, mtime, hash FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                (
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ),
            ))
        })?;

        let mut map = HashMap::new();
        for r in rows {
            let (path, data) = r?;
            map.insert(path, data);
        }
        Ok(map)
    }

    /// Atomically apply file updates, path deletions, and optional full alias
    /// swap + `meta.alias_fingerprint` update AND load all symbols inside a
    /// single `BEGIN IMMEDIATE` transaction. Eliminates empty-table visibility
    /// windows under concurrent readers.
    pub fn commit_sync_and_load(
        &mut self,
        full_replace: bool,
        new_alias_fp: Option<&str>,
        updates: &[(&str, i64, &str, &[Symbol])],
        deleted_paths: &[&str],
    ) -> Result<Vec<Symbol>> {
        if !full_replace && new_alias_fp.is_none() && updates.is_empty() && deleted_paths.is_empty()
        {
            return self.load_all_symbols();
        }

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        if full_replace {
            tx.execute("DELETE FROM files", [])?;
        } else {
            let mut del_stmt = tx.prepare_cached("DELETE FROM files WHERE path = ?")?;
            for &path in deleted_paths {
                del_stmt.execute(params![path])?;
            }
        }

        {
            let mut del_stmt = tx.prepare_cached("DELETE FROM files WHERE path = ?")?;
            let mut ins_file_stmt = tx.prepare_cached(
                "INSERT INTO files (path, mtime, hash, external_imports_json) VALUES (?, ?, ?, ?)",
            )?;
            let mut ins_sym_stmt = tx.prepare_cached(
                "INSERT INTO symbols (file_id, name, kind, start_line, end_line, signature, references_json, mentions_json, call_counts_json, member_calls_json, flags)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;

            for &(file_path, mtime, hash, symbols) in updates {
                if !full_replace {
                    del_stmt.execute(params![file_path])?;
                }
                let imports_json = symbols
                    .first()
                    .and_then(|first| serde_json::to_string(first.external_imports.as_ref()).ok())
                    .unwrap_or_else(|| "[]".to_string());
                ins_file_stmt.execute(params![file_path, mtime, hash, imports_json])?;
                let file_id = tx.last_insert_rowid();

                for s in symbols {
                    let calls_json =
                        serde_json::to_string(&s.calls).unwrap_or_else(|_| "[]".to_string());
                    let mentions_json =
                        serde_json::to_string(&s.mentions).unwrap_or_else(|_| "[]".to_string());
                    let counts_json =
                        serde_json::to_string(&s.call_counts).unwrap_or_else(|_| "{}".to_string());
                    let member_json =
                        serde_json::to_string(&s.member_calls).unwrap_or_else(|_| "[]".to_string());
                    ins_sym_stmt.execute(params![
                        file_id,
                        s.name,
                        s.kind.as_str(),
                        s.start_line as i64,
                        s.end_line as i64,
                        s.signature,
                        calls_json,
                        mentions_json,
                        counts_json,
                        member_json,
                        s.flags as i64,
                    ])?;
                }
            }
        }

        if let Some(fp) = new_alias_fp {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES ('alias_fingerprint', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![fp],
            )?;
        }

        let symbols = load_all_symbols_conn(&tx)?;
        tx.commit()?;
        Ok(symbols)
    }

    pub fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        load_all_symbols_conn(&self.conn)
    }
}

fn schema_is_current_tx(tx: &Transaction<'_>) -> Result<bool> {
    let found: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found != PARSER_VERSION {
        return Ok(false);
    }
    let table_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('files', 'symbols', 'meta')",
        [],
        |row| row.get(0),
    )?;
    Ok(table_count == 3)
}

fn load_all_symbols_conn(conn: &Connection) -> Result<Vec<Symbol>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature,
                s.references_json, s.mentions_json, s.call_counts_json, s.member_calls_json,
                f.external_imports_json, s.flags
         FROM symbols s JOIN files f ON s.file_id = f.id
         ORDER BY f.path, s.start_line, s.end_line, s.name, s.id",
    )?;
    let mut last_file_imports: Option<(String, Arc<[String]>)> = None;
    let mut rows = stmt.query([])?;
    let mut symbols = Vec::new();
    while let Some(row) = rows.next()? {
        let kind = parse_symbol_kind(row.get(1)?);
        let file: String = row.get(2)?;
        let json_vec = |idx: usize| -> rusqlite::Result<Vec<String>> {
            let s: String = row.get(idx)?;
            serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    idx,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        };
        let calls = json_vec(6)?;
        let mentions = json_vec(7)?;
        let call_counts_str: String = row.get(8)?;
        let call_counts: BTreeMap<String, u32> =
            serde_json::from_str(&call_counts_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let member_calls = json_vec(9)?;
        let external_imports = match &last_file_imports {
            Some((prev_file, arc)) if *prev_file == file => Arc::clone(arc),
            _ => {
                let imports_raw: String = row.get(10)?;
                let parsed: Vec<String> = serde_json::from_str(&imports_raw).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let arc: Arc<[String]> = parsed.into();
                last_file_imports = Some((file.clone(), Arc::clone(&arc)));
                arc
            }
        };
        let flags = row.get::<_, i64>(11)?.clamp(0, 255) as u8;
        symbols.push(Symbol {
            name: row.get(0)?,
            kind,
            file,
            start_line: row.get::<_, i64>(3)? as usize,
            end_line: row.get::<_, i64>(4)? as usize,
            signature: row.get(5)?,
            body: String::new(),
            centrality: 0.0,
            calls,
            mentions,
            call_counts,
            member_calls,
            external_imports,
            flags,
        });
    }
    Ok(symbols)
}

pub fn is_corruption_error(err: &anyhow::Error) -> bool {
    if let Some(sql_err) = err.downcast_ref::<rusqlite::Error>() {
        match sql_err {
            rusqlite::Error::SqliteFailure(e, _) => {
                matches!(
                    e.code,
                    ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt | ErrorCode::SchemaChanged
                )
            }
            rusqlite::Error::FromSqlConversionFailure(_, _, _) => true,
            _ => false,
        }
    } else {
        let msg = err.to_string().to_lowercase();
        msg.contains("not a database") || msg.contains("malformed") || msg.contains("corrupt")
    }
}

pub fn is_readonly_or_io_error(err: &anyhow::Error) -> bool {
    if let Some(rusqlite::Error::SqliteFailure(e, _)) = err.downcast_ref::<rusqlite::Error>() {
        return matches!(
            e.code,
            ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::PermissionDenied
        );
    }
    if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
        return io_err.kind() == std::io::ErrorKind::PermissionDenied
            || io_err.kind() == std::io::ErrorKind::ReadOnlyFilesystem;
    }
    let msg = err.to_string().to_lowercase();
    msg.contains("readonly") || msg.contains("permission denied") || msg.contains("read-only")
}

pub fn unlink_db_artifacts(db_path: &Path) {
    let base = db_path.as_os_str().to_os_string();
    for suffix in ["", "-wal", "-shm"] {
        let mut p = base.clone();
        p.push(suffix);
        let path = Path::new(&p);
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if !meta.file_type().is_symlink() && meta.is_file() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn parse_symbol_kind(raw: String) -> SymbolKind {
    match raw.as_str() {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "struct" => SymbolKind::Struct,
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "trait" => SymbolKind::Trait,
        "enum" => SymbolKind::Enum,
        "type" => SymbolKind::TypeAlias,
        "variable" => SymbolKind::Variable,
        "constant" => SymbolKind::Constant,
        "field" => SymbolKind::Field,
        _ => SymbolKind::Module,
    }
}
