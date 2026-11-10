use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Arc;

/// Bumped whenever parsing or the stored symbol/reference format changes.
///
/// v14: body-free and centrality-free `symbols` schema; canonical path ordering;
/// polyglot AST & edge-resolution fixes.
pub const PARSER_VERSION: i64 = 14;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open_or_create(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        // Set busy_timeout FIRST so concurrent cold opens wait instead of
        // failing immediately with SQLITE_BUSY during WAL/schema setup.
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )?;

        let mut db = Database { conn };
        db.ensure_schema_and_version()?;
        Ok(db)
    }

    /// Initialize or migrate schema only when `user_version != PARSER_VERSION`
    /// or tables are missing, avoiding DDL write locks on warm opens.
    fn ensure_schema_and_version(&mut self) -> Result<()> {
        let found: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let has_tables: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'symbols'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        if found == PARSER_VERSION && has_tables {
            return Ok(());
        }

        let _ = self.conn.execute_batch("PRAGMA journal_mode = WAL;");
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS files;

             CREATE TABLE files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT UNIQUE NOT NULL,
                 mtime INTEGER NOT NULL,
                 hash TEXT NOT NULL
             );

             CREATE TABLE symbols (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 file_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 signature TEXT NOT NULL,
                 references_json TEXT DEFAULT '[]',
                 mentions_json TEXT DEFAULT '[]',
                 call_counts_json TEXT DEFAULT '{}',
                 member_calls_json TEXT DEFAULT '[]',
                 external_imports_json TEXT DEFAULT '[]',
                 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
             );

             CREATE INDEX idx_symbols_name ON symbols(name);
             CREATE INDEX idx_symbols_file_id ON symbols(file_id);

             CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             DELETE FROM meta;",
        )?;
        tx.execute_batch(&format!("PRAGMA user_version = {PARSER_VERSION};"))?;
        tx.commit()?;
        Ok(())
    }

    /// Drop the cached parse when the workspace's alias config changed.
    pub fn enforce_alias_fingerprint(&mut self, fingerprint: &str) -> Result<()> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'alias_fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref() == Some(fingerprint) {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM files", [])?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('alias_fingerprint', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![fingerprint],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_file_records(
        &self,
    ) -> Result<std::collections::HashMap<String, (i64, i64, String)>> {
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

        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (path, data) = r?;
            map.insert(path, data);
        }
        Ok(map)
    }

    pub fn save_batch(
        &mut self,
        updates: &[(&str, i64, &str, &[Symbol])],
        deleted_file_ids: &[i64],
    ) -> Result<()> {
        if updates.is_empty() && deleted_file_ids.is_empty() {
            return Ok(());
        }

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        for &file_id in deleted_file_ids {
            tx.execute("DELETE FROM files WHERE id = ?", params![file_id])?;
        }

        {
            let mut del_stmt = tx.prepare_cached("DELETE FROM files WHERE path = ?")?;
            let mut ins_file_stmt =
                tx.prepare_cached("INSERT INTO files (path, mtime, hash) VALUES (?, ?, ?)")?;
            let mut ins_sym_stmt = tx.prepare_cached(
                "INSERT INTO symbols (file_id, name, kind, start_line, end_line, signature, references_json, mentions_json, call_counts_json, member_calls_json, external_imports_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;

            for &(file_path, mtime, hash, symbols) in updates {
                del_stmt.execute(params![file_path])?;
                ins_file_stmt.execute(params![file_path, mtime, hash])?;
                let file_id = tx.last_insert_rowid();

                // External imports are file-wide; serialize once per file when shared.
                let shared_imports_json = symbols.first().map(|first| {
                    serde_json::to_string(first.external_imports.as_ref())
                        .unwrap_or_else(|_| "[]".to_string())
                });

                for s in symbols {
                    // references_json stores calls (column name kept for schema stability).
                    let calls_json =
                        serde_json::to_string(&s.calls).unwrap_or_else(|_| "[]".to_string());
                    let mentions_json =
                        serde_json::to_string(&s.mentions).unwrap_or_else(|_| "[]".to_string());
                    let counts_json =
                        serde_json::to_string(&s.call_counts).unwrap_or_else(|_| "{}".to_string());
                    let member_json =
                        serde_json::to_string(&s.member_calls).unwrap_or_else(|_| "[]".to_string());
                    let imports_json = match (&shared_imports_json, symbols.first()) {
                        (Some(cached), Some(first))
                            if Arc::ptr_eq(&first.external_imports, &s.external_imports) =>
                        {
                            cached.clone()
                        }
                        _ => serde_json::to_string(s.external_imports.as_ref())
                            .unwrap_or_else(|_| "[]".to_string()),
                    };
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
                        imports_json
                    ])?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn save_file_and_symbols(
        &mut self,
        file_path: &str,
        mtime: i64,
        hash: &str,
        symbols: &[Symbol],
    ) -> Result<()> {
        self.save_batch(&[(file_path, mtime, hash, symbols)], &[])
    }

    pub fn delete_file_by_id(&self, file_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE id = ?", params![file_id])?;
        Ok(())
    }

    pub fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.references_json, s.mentions_json, s.call_counts_json, s.member_calls_json, s.external_imports_json
             FROM symbols s JOIN files f ON s.file_id = f.id
             ORDER BY f.path, s.start_line, s.end_line, s.name, s.id",
        )?;
        let mut last_file_imports: Option<(String, String, Arc<[String]>)> = None;
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
            let call_counts: std::collections::HashMap<String, u32> =
                serde_json::from_str(&call_counts_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let member_calls = json_vec(9)?;
            let imports_raw: String = row.get(10)?;
            let external_imports = match &last_file_imports {
                Some((prev_file, prev_raw, arc))
                    if *prev_file == file && *prev_raw == imports_raw =>
                {
                    Arc::clone(arc)
                }
                _ => {
                    let parsed: Vec<String> = serde_json::from_str(&imports_raw).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    let arc: Arc<[String]> = parsed.into();
                    last_file_imports = Some((file.clone(), imports_raw, Arc::clone(&arc)));
                    arc
                }
            };
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
            });
        }
        Ok(symbols)
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
