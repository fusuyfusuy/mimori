use crate::model::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

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
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;

             CREATE TABLE IF NOT EXISTS files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT UNIQUE NOT NULL,
                 mtime INTEGER NOT NULL,
                 hash TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS symbols (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 file_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 signature TEXT NOT NULL,
                 body TEXT NOT NULL,
                 centrality REAL DEFAULT 0.0,
                 references_json TEXT DEFAULT '[]',
                 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
             );

             CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
             CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
             CREATE INDEX IF NOT EXISTS idx_symbols_centrality ON symbols(centrality DESC);

             CREATE TABLE IF NOT EXISTS edges (
                 caller_id INTEGER NOT NULL,
                 callee_id INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 PRIMARY KEY (caller_id, callee_id, kind),
                 FOREIGN KEY(caller_id) REFERENCES symbols(id) ON DELETE CASCADE,
                 FOREIGN KEY(callee_id) REFERENCES symbols(id) ON DELETE CASCADE
             );

             CREATE INDEX IF NOT EXISTS idx_edges_caller ON edges(caller_id);
             CREATE INDEX IF NOT EXISTS idx_edges_callee ON edges(callee_id);",
        )?;

        Ok(Database { conn })
    }

    pub fn get_file_records(&self) -> Result<std::collections::HashMap<String, (i64, i64, String)>> {
        let mut stmt = self.conn.prepare("SELECT id, path, mtime, hash FROM files")?;
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

    pub fn save_file_and_symbols(
        &mut self,
        file_path: &str,
        mtime: i64,
        hash: &str,
        symbols: &[Symbol],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Delete existing file entry if present
        tx.execute("DELETE FROM files WHERE path = ?", params![file_path])?;

        tx.execute(
            "INSERT INTO files (path, mtime, hash) VALUES (?, ?, ?)",
            params![file_path, mtime, hash],
        )?;
        let file_id = tx.last_insert_rowid();

        for s in symbols {
            let refs_json = serde_json::to_string(&s.references).unwrap_or_else(|_| "[]".to_string());
            tx.execute(
                "INSERT INTO symbols (file_id, name, kind, start_line, end_line, signature, body, centrality, references_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    file_id,
                    s.name,
                    s.kind.as_str(),
                    s.start_line as i64,
                    s.end_line as i64,
                    s.signature,
                    s.body,
                    s.centrality,
                    refs_json
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn delete_file_by_id(&self, file_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM files WHERE id = ?", params![file_id])?;
        Ok(())
    }

    pub fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, f.path, s.start_line, s.end_line, s.signature, s.body, s.centrality, s.references_json
             FROM symbols s JOIN files f ON s.file_id = f.id
             ORDER BY s.centrality DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let kind_str: String = row.get(1)?;
            let kind = match kind_str.as_str() {
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
                _ => SymbolKind::Module,
            };

            let refs_str: String = row.get(8)?;
            let references: Vec<String> = serde_json::from_str(&refs_str).unwrap_or_default();

            Ok(Symbol {
                name: row.get(0)?,
                kind,
                file: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                signature: row.get(5)?,
                body: row.get(6)?,
                doc: None,
                centrality: row.get(7)?,
                references,
            })
        })?;

        let mut symbols = Vec::new();
        for r in rows {
            symbols.push(r?);
        }
        Ok(symbols)
    }

    pub fn update_centralities(&mut self, symbols: &[Symbol]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for s in symbols {
            tx.execute(
                "UPDATE symbols SET centrality = ? WHERE name = ? AND file_id = (SELECT id FROM files WHERE path = ?)",
                params![s.centrality, s.name, s.file],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
