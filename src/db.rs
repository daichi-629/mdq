use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};

use crate::markdown::{normalize_target, parse_note};
use crate::model::{EmbeddingInput, LinkRef, NoteRef, PageRecord, SearchHit};
use crate::query::MetadataFilter;
use crate::tokenize::{fts_query, index_text};

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open index {}", path.display()))?;
        connection.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                frontmatter_json TEXT,
                mtime INTEGER NOT NULL,
                ctime INTEGER NOT NULL DEFAULT 0,
                size INTEGER NOT NULL,
                content_hash TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY,
                note_id INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                heading TEXT,
                body TEXT NOT NULL,
                search_text TEXT NOT NULL DEFAULT '',
                content_hash TEXT NOT NULL DEFAULT ''
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                search_text,
                content='chunks',
                content_rowid='id',
                tokenize='unicode61 remove_diacritics 2'
            );
            CREATE TABLE IF NOT EXISTS links (
                id INTEGER PRIMARY KEY,
                source_note_id INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                raw_target TEXT NOT NULL,
                normalized_target TEXT NOT NULL,
                target_note_id INTEGER REFERENCES notes(id) ON DELETE SET NULL,
                heading TEXT,
                is_embed INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS links_source_idx ON links(source_note_id);
            CREATE INDEX IF NOT EXISTS links_target_idx ON links(target_note_id);
            CREATE TABLE IF NOT EXISTS embeddings (
                content_hash TEXT NOT NULL,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector BLOB NOT NULL,
                PRIMARY KEY(content_hash, model)
            );
            ",
        )?;
        ensure_column(
            &connection,
            "chunks",
            "search_text",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &connection,
            "chunks",
            "content_hash",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&connection, "notes", "ctime", "INTEGER NOT NULL DEFAULT 0")?;
        Ok(Self { connection })
    }

    pub fn rebuild(&mut self, vault: &Path) -> Result<IndexStats> {
        let transaction = self.connection.transaction()?;
        transaction.execute_batch(
            "
            DELETE FROM chunks_fts;
            DELETE FROM links;
            DELETE FROM chunks;
            DELETE FROM notes;
            ",
        )?;

        let files: Vec<PathBuf> = WalkDir::new(vault)
            .follow_links(false)
            .into_iter()
            .filter_entry(visible_entry)
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            })
            .map(DirEntry::into_path)
            .collect();

        let mut chunk_count = 0;
        let mut link_count = 0;
        for file in &files {
            let note = parse_note(vault, file)?;
            transaction.execute(
                "INSERT INTO notes(path, title, body, frontmatter_json, mtime, ctime, size, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    note.path,
                    note.title,
                    note.body,
                    note.frontmatter.as_ref().map(serde_json::Value::to_string),
                    note.mtime,
                    note.ctime,
                    note.size as i64,
                    note.hash,
                ],
            )?;
            let note_id = transaction.last_insert_rowid();
            for chunk in note.chunks {
                let searchable = format!(
                    "{} {} {}",
                    note.title,
                    chunk.heading.as_deref().unwrap_or_default(),
                    chunk.body
                );
                let search_text = index_text(&searchable);
                let content_hash = hex::encode(Sha256::digest(searchable.as_bytes()));
                transaction.execute(
                    "INSERT INTO chunks(
                        note_id, ordinal, heading, body, search_text, content_hash
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        note_id,
                        chunk.ordinal as i64,
                        chunk.heading,
                        chunk.body,
                        search_text,
                        content_hash,
                    ],
                )?;
                let chunk_id = transaction.last_insert_rowid();
                transaction.execute(
                    "INSERT INTO chunks_fts(rowid, search_text) VALUES (?1, ?2)",
                    params![chunk_id, index_text(&searchable)],
                )?;
                chunk_count += 1;
            }
            for link in note.links {
                transaction.execute(
                    "INSERT INTO links(
                        source_note_id, raw_target, normalized_target, heading, is_embed
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        note_id,
                        link.raw_target,
                        link.target,
                        link.heading,
                        link.is_embed,
                    ],
                )?;
                link_count += 1;
            }
        }

        resolve_links(&transaction)?;
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('vault', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [canonical(vault)?.to_string_lossy().as_ref()],
        )?;
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('indexed_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        transaction.commit()?;
        Ok(IndexStats {
            notes: files.len(),
            chunks: chunk_count,
            links: link_count,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let match_query = fts_query(query);
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "
            SELECT c.id, n.path, n.title, c.heading, bm25(chunks_fts) AS rank, c.body
            FROM chunks_fts
            JOIN chunks c ON c.id = chunks_fts.rowid
            JOIN notes n ON n.id = c.note_id
            WHERE chunks_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            ",
        )?;
        let rows = statement.query_map(params![match_query, limit as i64], |row| {
            let body: String = row.get(5)?;
            Ok(SearchHit {
                chunk_id: row.get(0)?,
                path: row.get(1)?,
                title: row.get(2)?,
                heading: row.get(3)?,
                score: -row.get::<_, f64>(4)?,
                snippet: source_snippet(&body, query, 180),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn missing_embeddings(&self, model: &str) -> Result<Vec<EmbeddingInput>> {
        let mut statement = self.connection.prepare(
            "
            SELECT DISTINCT c.content_hash, n.title, c.heading, c.body
            FROM chunks c
            JOIN notes n ON n.id = c.note_id
            LEFT JOIN embeddings e
              ON e.content_hash = c.content_hash AND e.model = ?1
            WHERE e.content_hash IS NULL
            ORDER BY c.id
            ",
        )?;
        let rows = statement.query_map([model], |row| {
            let title: String = row.get(1)?;
            let heading: Option<String> = row.get(2)?;
            let body: String = row.get(3)?;
            Ok(EmbeddingInput {
                content_hash: row.get(0)?,
                text: format!(
                    "passage: {}\n{}\n{}",
                    title,
                    heading.as_deref().unwrap_or_default(),
                    body
                ),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn store_embeddings(&mut self, model: &str, items: &[(String, Vec<f32>)]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for (content_hash, vector) in items {
            let blob = vector_to_blob(vector);
            transaction.execute(
                "
                INSERT INTO embeddings(content_hash, model, dimensions, vector)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(content_hash, model) DO UPDATE SET
                    dimensions = excluded.dimensions,
                    vector = excluded.vector
                ",
                params![content_hash, model, vector.len() as i64, blob],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn semantic_search(
        &self,
        model: &str,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut statement = self.connection.prepare(
            "
            SELECT c.id, n.path, n.title, c.heading, c.body, e.vector
            FROM chunks c
            JOIN notes n ON n.id = c.note_id
            JOIN embeddings e ON e.content_hash = c.content_hash
            WHERE e.model = ?1 AND e.dimensions = ?2
            ",
        )?;
        let rows = statement.query_map(params![model, query_vector.len() as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (chunk_id, path, title, heading, body, blob) = row?;
            let vector = blob_to_vector(&blob);
            hits.push(SearchHit {
                chunk_id,
                path,
                title,
                heading,
                score: dot_product(query_vector, &vector),
                snippet: source_snippet(&body, "", 180),
            });
        }
        hits.sort_by(|left, right| right.score.total_cmp(&left.score));
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn chunk_body(&self, chunk_id: i64) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT body FROM chunks WHERE id = ?1", [chunk_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn query_frontmatter(&self, expression: &dyn MetadataFilter) -> Result<Vec<NoteRef>> {
        let mut statement = self.connection.prepare(
            "SELECT path, title, frontmatter_json FROM notes WHERE frontmatter_json IS NOT NULL ORDER BY path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut matches = Vec::new();
        for row in rows {
            let (path, file_stem_title, json) = row?;
            let value: Option<serde_json::Value> = serde_json::from_str(&json).ok();
            if value.as_ref().is_some_and(|v| expression.matches(v)) {
                // Prefer the frontmatter `title` field so that the JSON output is consistent
                // with what the filter expression matched against.
                let title = value
                    .as_ref()
                    .and_then(|v| v["title"].as_str())
                    .map(str::to_owned)
                    .unwrap_or(file_stem_title);
                matches.push(NoteRef { path, title });
            }
        }
        Ok(matches)
    }

    pub fn all_chunks(&self) -> Result<Vec<SearchHit>> {
        let mut statement = self.connection.prepare(
            "
            SELECT c.id, n.path, n.title, c.heading, c.body
            FROM chunks c
            JOIN notes n ON n.id = c.note_id
            ORDER BY c.id
            ",
        )?;
        let rows = statement.query_map([], |row| {
            let body: String = row.get(4)?;
            Ok(SearchHit {
                chunk_id: row.get(0)?,
                path: row.get(1)?,
                title: row.get(2)?,
                heading: row.get(3)?,
                score: 0.0,
                snippet: source_snippet(&body, "", 180),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Returns format-neutral page records for compatibility query adapters.
    pub fn all_pages(&self) -> Result<Vec<PageRecord>> {
        let mut statement = self.connection.prepare(
            "
            SELECT path, title, body, frontmatter_json, mtime, ctime, size
            FROM notes
            ORDER BY path
            ",
        )?;
        let rows = statement.query_map([], |row| {
            let metadata = row
                .get::<_, Option<String>>(3)?
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
            Ok(PageRecord {
                path: row.get(0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                metadata,
                mtime: row.get(4)?,
                ctime: row.get(5)?,
                size: row.get::<_, i64>(6)? as u64,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn backlinks(&self, target: &str) -> Result<Vec<LinkRef>> {
        let target_id = self.resolve_note_id(target)?;
        let Some(target_id) = target_id else {
            return Ok(Vec::new());
        };
        self.link_query(
            "
            SELECT s.path, s.title, t.path, t.title, l.raw_target, l.heading, l.is_embed
            FROM links l
            JOIN notes s ON s.id = l.source_note_id
            LEFT JOIN notes t ON t.id = l.target_note_id
            WHERE l.target_note_id = ?1
            ORDER BY s.path
            ",
            target_id,
        )
    }

    /// Every link in the vault, for bulk construction of compatibility
    /// `file.links` / `file.backlinks` / `file.embeds` fields without N+1 queries.
    pub fn all_links(&self) -> Result<Vec<LinkRef>> {
        let mut statement = self.connection.prepare(
            "
            SELECT s.path, s.title, t.path, t.title, l.raw_target, l.heading, l.is_embed
            FROM links l
            JOIN notes s ON s.id = l.source_note_id
            LEFT JOIN notes t ON t.id = l.target_note_id
            ORDER BY s.path, l.id
            ",
        )?;
        let rows = statement.query_map([], |row| {
            let target_path: Option<String> = row.get(2)?;
            let target_title: Option<String> = row.get(3)?;
            Ok(LinkRef {
                source: NoteRef {
                    path: row.get(0)?,
                    title: row.get(1)?,
                },
                target: target_path
                    .zip(target_title)
                    .map(|(path, title)| NoteRef { path, title }),
                raw_target: row.get(4)?,
                heading: row.get(5)?,
                embed: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn outgoing_links(&self, source: &str) -> Result<Vec<LinkRef>> {
        let source_id = self.resolve_note_id(source)?;
        let Some(source_id) = source_id else {
            return Ok(Vec::new());
        };
        self.link_query(
            "
            SELECT s.path, s.title, t.path, t.title, l.raw_target, l.heading, l.is_embed
            FROM links l
            JOIN notes s ON s.id = l.source_note_id
            LEFT JOIN notes t ON t.id = l.target_note_id
            WHERE l.source_note_id = ?1
            ORDER BY l.id
            ",
            source_id,
        )
    }

    fn link_query(&self, sql: &str, note_id: i64) -> Result<Vec<LinkRef>> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map([note_id], |row| {
            let target_path: Option<String> = row.get(2)?;
            let target_title: Option<String> = row.get(3)?;
            Ok(LinkRef {
                source: NoteRef {
                    path: row.get(0)?,
                    title: row.get(1)?,
                },
                target: target_path
                    .zip(target_title)
                    .map(|(path, title)| NoteRef { path, title }),
                raw_target: row.get(4)?,
                heading: row.get(5)?,
                embed: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn note_body(&self, path: &str) -> Result<Option<(NoteRef, String)>> {
        let id = self.resolve_note_id(path)?;
        let Some(id) = id else {
            return Ok(None);
        };
        self.connection
            .query_row(
                "SELECT path, title, body FROM notes WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        NoteRef {
                            path: row.get(0)?,
                            title: row.get(1)?,
                        },
                        row.get(2)?,
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn status(&self, embedding_model: &str) -> Result<Status> {
        let count = |table: &str| -> Result<usize> {
            Ok(self
                .connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })? as usize)
        };
        let vault = self.metadata("vault")?;
        let index_stale = match &vault {
            Some(path) => self.is_stale(Path::new(path))?,
            None => true,
        };
        Ok(Status {
            has_index: vault.is_some(),
            vault,
            indexed_at: self.metadata("indexed_at")?,
            notes: count("notes")?,
            chunks: count("chunks")?,
            links: count("links")?,
            unresolved_links: self.connection.query_row(
                "SELECT count(*) FROM links WHERE target_note_id IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )? as usize,
            embeddings: self.connection.query_row(
                "
                SELECT count(*)
                FROM chunks c
                WHERE EXISTS (
                    SELECT 1 FROM embeddings e WHERE e.content_hash = c.content_hash
                )
                ",
                [],
                |row| row.get::<_, i64>(0),
            )? as usize,
            cached_embeddings: count("embeddings")?,
            index_stale,
            embeddings_stale: self.embeddings_stale(embedding_model)?,
        })
    }

    /// True when the vault's Markdown files have changed since the last `rebuild`.
    pub fn is_stale(&self, vault: &Path) -> Result<bool> {
        Ok(self.staleness(vault)? > 0)
    }

    /// Number of Markdown files added, removed, or modified since the last `rebuild`.
    pub fn staleness(&self, vault: &Path) -> Result<usize> {
        let mut current = HashMap::<String, (i64, u64)>::new();
        for entry in WalkDir::new(vault)
            .follow_links(false)
            .into_iter()
            .filter_entry(visible_entry)
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            })
        {
            let metadata = entry.metadata()?;
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or_default();
            let relative = entry
                .path()
                .strip_prefix(vault)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            current.insert(relative, (mtime, metadata.len()));
        }

        let mut statement = self
            .connection
            .prepare("SELECT path, mtime, size FROM notes")?;
        let mut indexed = HashMap::<String, (i64, u64)>::new();
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)? as u64,
            ))
        })?;
        for row in rows {
            let (path, mtime, size) = row?;
            indexed.insert(path, (mtime, size));
        }

        let mut changed = 0;
        let all_paths: HashSet<&String> = current.keys().chain(indexed.keys()).collect();
        for path in all_paths {
            if current.get(path) != indexed.get(path) {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// True when chunks exist that have not yet been embedded with `model`.
    pub fn embeddings_stale(&self, model: &str) -> Result<bool> {
        Ok(self.missing_embeddings_count(model)? > 0)
    }

    /// Number of chunks that have not yet been embedded with `model`.
    pub fn missing_embeddings_count(&self, model: &str) -> Result<usize> {
        let missing: i64 = self.connection.query_row(
            "
            SELECT count(*)
            FROM chunks c
            LEFT JOIN embeddings e
              ON e.content_hash = c.content_hash AND e.model = ?1
            WHERE e.content_hash IS NULL
            ",
            [model],
            |row| row.get(0),
        )?;
        Ok(missing as usize)
    }

    /// True when this vault has a previously built index (as opposed to never indexed).
    pub fn has_index(&self) -> Result<bool> {
        Ok(self.metadata("vault")?.is_some())
    }

    /// True when at least one embedding has been cached for this vault.
    pub fn has_embeddings(&self) -> Result<bool> {
        Ok(self
            .connection
            .query_row("SELECT count(*) FROM embeddings", [], |row| {
                row.get::<_, i64>(0)
            })?
            > 0)
    }

    fn metadata(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    /// Returns the path of the vault that was last indexed into this database.
    pub fn indexed_vault(&self) -> Result<Option<String>> {
        self.metadata("vault")
    }

    fn resolve_note_id(&self, input: &str) -> Result<Option<i64>> {
        let normalized = normalize_target(input);
        let filename = normalized.rsplit('/').next().unwrap_or(&normalized);
        // Try exact path match first — this is always unambiguous even when another note
        // shares the same filename or title.
        let mut path_stmt = self.connection.prepare(
            "
            SELECT id FROM notes
            WHERE lower(path) = ?1 || '.md'
               OR lower(substr(path, 1, length(path) - 3)) = ?1
            LIMIT 2
            ",
        )?;
        let path_ids = path_stmt
            .query_map([&normalized], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        if path_ids.len() == 1 {
            return Ok(Some(path_ids[0]));
        }
        // Fall back to title / filename match (may be ambiguous).
        let mut title_stmt = self
            .connection
            .prepare("SELECT id FROM notes WHERE lower(title) = ?1 LIMIT 2")?;
        let title_ids = title_stmt
            .query_map([filename], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok((title_ids.len() == 1).then(|| title_ids[0]))
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|existing| existing == column) {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

fn resolve_links(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut by_path = HashMap::<String, Vec<i64>>::new();
    let mut by_title = HashMap::<String, Vec<i64>>::new();
    {
        let mut statement = transaction.prepare("SELECT id, path, title FROM notes")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, path, title) = row?;
            let without_extension = path.strip_suffix(".md").unwrap_or(&path).to_lowercase();
            by_path.entry(without_extension).or_default().push(id);
            by_title.entry(title.to_lowercase()).or_default().push(id);
        }
    }
    let mut links = Vec::new();
    {
        let mut statement = transaction.prepare(
            "
            SELECT l.id, l.normalized_target, n.path
            FROM links l
            JOIN notes n ON n.id = l.source_note_id
            WHERE l.target_note_id IS NULL
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            links.push(row?);
        }
    }
    for (link_id, target, source_path) in links {
        let mut target_forms = vec![collapse_path(&target)];
        let source_parent = source_path.rsplit_once('/').map(|(parent, _)| parent);
        if let Some(parent) = source_parent {
            target_forms.push(collapse_path(&format!("{parent}/{target}")));
        }
        target_forms.dedup();

        let filename = target_forms[0]
            .rsplit('/')
            .next()
            .unwrap_or(&target_forms[0]);
        let exact = target_forms
            .iter()
            .find_map(|form| by_path.get(form).filter(|ids| ids.len() == 1));
        let candidates = exact.or_else(|| by_title.get(filename).filter(|ids| ids.len() == 1));
        if let Some(candidates) = candidates {
            transaction.execute(
                "UPDATE links SET target_note_id = ?1 WHERE id = ?2",
                params![candidates[0], link_id],
            )?;
        }
    }
    Ok(())
}

fn collapse_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/").to_lowercase()
}

fn source_snippet(body: &str, query: &str, max_chars: usize) -> String {
    let body_chars: Vec<char> = body.chars().collect();
    if body_chars.len() <= max_chars {
        return body.replace('\n', " ");
    }

    let query_lower = query.to_lowercase();
    let body_lower = body.to_lowercase();
    let byte_position = body_lower.find(&query_lower).unwrap_or(0);
    let character_position = body[..byte_position].chars().count();
    let half = max_chars / 2;
    let start = character_position.saturating_sub(half);
    let end = (start + max_chars).min(body_chars.len());
    let mut snippet: String = body_chars[start..end].iter().collect();
    snippet = snippet.replace('\n', " ");
    if start > 0 {
        snippet.insert_str(0, "… ");
    }
    if end < body_chars.len() {
        snippet.push_str(" …");
    }
    snippet
}

fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

fn dot_product(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| (*left as f64) * (*right as f64))
        .sum()
}

fn visible_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !name.starts_with('.') && name != "node_modules" && name != "target"
}

pub fn default_db_path(vault: &Path) -> Result<PathBuf> {
    let canonical = canonical(vault)?;
    let digest = hex::encode(Sha256::digest(canonical.to_string_lossy().as_bytes()));
    let root = dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .context("cannot determine cache directory")?;
    Ok(root.join("mdq").join(&digest[..16]).join("index.sqlite3"))
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("path does not exist: {}", path.display()))
}

pub struct IndexStats {
    pub notes: usize,
    pub chunks: usize,
    pub links: usize,
}

#[derive(serde::Serialize)]
pub struct Status {
    pub vault: Option<String>,
    pub indexed_at: Option<String>,
    pub has_index: bool,
    pub notes: usize,
    pub chunks: usize,
    pub links: usize,
    pub unresolved_links: usize,
    pub embeddings: usize,
    pub cached_embeddings: usize,
    pub index_stale: bool,
    pub embeddings_stale: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Expression;
    use std::fs;

    #[test]
    fn indexes_generic_frontmatter_and_both_link_styles() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("notes");
        fs::create_dir_all(vault.join("nested")).unwrap();
        fs::write(
            vault.join("alpha.md"),
            "---\n任意項目:\n  状態: 有効\n---\n# Alpha\n[[nested/beta]]\n",
        )
        .unwrap();
        fs::write(
            vault.join("nested/beta.md"),
            "# Beta\n[Alpha](../alpha.md)\n検索対象の日本語本文\n",
        )
        .unwrap();

        let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
        let stats = database.rebuild(&vault).unwrap();
        assert_eq!(stats.notes, 2);
        assert_eq!(stats.links, 2);

        let expression = Expression::parse("任意項目.状態 = 有効").unwrap();
        let notes = database.query_frontmatter(&expression).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].path, "alpha.md");

        let alpha_backlinks = database.backlinks("alpha").unwrap();
        assert_eq!(alpha_backlinks.len(), 1);
        assert_eq!(alpha_backlinks[0].source.path, "nested/beta.md");

        let beta_backlinks = database.backlinks("nested/beta").unwrap();
        assert_eq!(beta_backlinks.len(), 1);
        assert_eq!(beta_backlinks[0].source.path, "alpha.md");

        let hits = database.search("日本語", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "nested/beta.md");
    }

    #[test]
    fn query_frontmatter_finds_frontmatter_only_notes() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("notes");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("meta_only.md"), "---\nkind: reference\n---\n").unwrap();
        fs::write(
            vault.join("with_body.md"),
            "---\nkind: reference\n---\n# Body\nsome content\n",
        )
        .unwrap();
        let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
        database.rebuild(&vault).unwrap();

        let expression = Expression::parse("kind = reference").unwrap();
        let notes = database.query_frontmatter(&expression).unwrap();
        assert_eq!(
            notes.len(),
            2,
            "frontmatter-only note must be returned by query_frontmatter"
        );
        let paths: Vec<&str> = notes.iter().map(|n| n.path.as_str()).collect();
        assert!(paths.contains(&"meta_only.md"));
        assert!(paths.contains(&"with_body.md"));
    }

    #[test]
    fn exact_path_resolves_despite_same_title_in_other_folder() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("notes");
        fs::create_dir_all(vault.join("a")).unwrap();
        fs::create_dir_all(vault.join("b")).unwrap();
        fs::write(vault.join("a/Alpha.md"), "# Alpha\n[[b/Alpha]]\n").unwrap();
        fs::write(vault.join("b/Alpha.md"), "# Alpha\n").unwrap();
        let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
        database.rebuild(&vault).unwrap();

        // Exact folder-qualified path must resolve even though both share the title "Alpha"
        let links = database.outgoing_links("a/Alpha").unwrap();
        assert_eq!(links.len(), 1);
        let backlinks = database.backlinks("b/Alpha").unwrap();
        assert_eq!(backlinks.len(), 1);
    }

    #[test]
    fn backlinks_returns_none_for_missing_note() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("notes");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("a.md"), "# A\n").unwrap();
        let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
        database.rebuild(&vault).unwrap();

        let result = database.note_body("nonexistent").unwrap();
        assert!(
            result.is_none(),
            "note_body must return None for missing note"
        );
    }
}
