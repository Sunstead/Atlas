//! The search index: derived data, rebuilt from the sources whenever it's
//! missing, stale or doubtful. Lives in `ATLAS_INDEX_DIR`
//! (`/srv/storage/derived/atlas`), never backed up.
//!
//! Two parts, kept in step:
//! - `index.db` (SQLite): one row per item (what results show: title, path,
//!   size, dates) plus each connection's sync state;
//! - `tantivy/`: full text, keyed by the row id, with the owner's user id on
//!   every document. Every search filters on it, so a user only ever sees
//!   their own items.
//!
//! Writes go through [`Batch`], which an indexed source fills as an
//! [`IndexSink`] and which flushes both parts together. If they ever
//! disagree (a crash between the two commits), [`Index::open`] notices and
//! starts over empty; the next syncs refill it.
//!
//! The API is blocking. Call it from `spawn_blocking`.

use atlas_core::{Doc, IndexSink, Known, SourceError};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{doc, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

/// Bump to rebuild every index on upgrade (schema or tokenizer changes).
const SCHEMA_VERSION: i64 = 1;

/// Rows per flush during a sync: bounds memory (each body can be 256 KB).
const FLUSH_EVERY: usize = 200;

const WRITER_HEAP: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("search index: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("index directory: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, IndexError>;

impl From<IndexError> for SourceError {
    fn from(e: IndexError) -> Self {
        SourceError::Other(e.to_string())
    }
}

#[derive(Clone, Copy)]
struct Fields {
    doc_id: Field,
    user: Field,
    connection: Field,
    kind: Field,
    title: Field,
    path: Field,
    body: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let numeric = NumericOptions::default().set_indexed().set_fast();
    let doc_id = b.add_u64_field("doc_id", numeric.clone().set_stored());
    let user = b.add_i64_field("user", numeric.clone());
    let connection = b.add_i64_field("connection", numeric);
    let kind = b.add_text_field("kind", STRING);
    // Stemmed English for prose: "taxes" finds "tax".
    let stemmed = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer("en_stem").set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let title = b.add_text_field("title", stemmed.clone());
    // Folder names are words too: a "Taxes" folder should match "tax".
    let path = b.add_text_field("path", stemmed.clone());
    // Stored for snippets; already capped at extraction.
    let body = b.add_text_field("body", stemmed | STORED);
    (b.build(), Fields { doc_id, user, connection, kind, title, path, body })
}

/// One item as the index holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct DocRow {
    pub id: i64,
    pub user_id: i64,
    pub connection_id: i64,
    pub external_id: String,
    pub kind: String,
    pub title: String,
    pub path: Option<String>,
    pub mime: Option<String>,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub fingerprint: Option<String>,
    pub indexed_at: i64,
}

const ROW: &str =
    "id, user_id, connection_id, external_id, kind, title, path, mime, size, mtime, fingerprint, indexed_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<DocRow> {
    Ok(DocRow {
        id: r.get(0)?,
        user_id: r.get(1)?,
        connection_id: r.get(2)?,
        external_id: r.get(3)?,
        kind: r.get(4)?,
        title: r.get(5)?,
        path: r.get(6)?,
        mime: r.get(7)?,
        size: r.get::<_, Option<i64>>(8)?.map(|s| s as u64),
        mtime: r.get(9)?,
        fingerprint: r.get(10)?,
        indexed_at: r.get(11)?,
    })
}

/// A search, already scoped to one user by the caller.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub kind: Option<String>,
    pub connection: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub row: DocRow,
    pub score: f32,
    /// A fragment of the body around the match, with byte ranges to
    /// highlight. `None` when the match was in the title or path only.
    pub snippet: Option<(String, Vec<(usize, usize)>)>,
}

/// A connection's last sync, as the settings page shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    pub last_full_at: Option<i64>,
    pub last_error: Option<String>,
}

pub struct Index {
    db: Mutex<Connection>,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    index: tantivy::Index,
    fields: Fields,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

const DB_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS documents (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id       INTEGER NOT NULL,
        connection_id INTEGER NOT NULL,
        external_id   TEXT    NOT NULL,
        kind          TEXT    NOT NULL,
        title         TEXT    NOT NULL,
        path          TEXT,
        mime          TEXT,
        size          INTEGER,
        mtime         INTEGER,
        fingerprint   TEXT,
        indexed_at    INTEGER NOT NULL,
        UNIQUE (connection_id, external_id)
    );
    CREATE INDEX IF NOT EXISTS documents_user ON documents (user_id);
    CREATE TABLE IF NOT EXISTS sync_state (
        connection_id INTEGER PRIMARY KEY,
        last_full_at  INTEGER,
        last_error    TEXT
    );
";

impl Index {
    /// Opens the index in `dir`, starting over if it's from another schema
    /// version or its two parts disagree.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        match Self::try_open(dir) {
            Ok(Some(index)) => Ok(index),
            Ok(None) => {
                tracing::warn!(dir = %dir.display(), "search index is stale; rebuilding it from the sources");
                Self::wipe(dir)?;
                Self::try_open(dir)?.ok_or_else(|| IndexError::Io(std::io::Error::other("fresh index is inconsistent")))
            }
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "can't open the search index; rebuilding it");
                Self::wipe(dir)?;
                Self::try_open(dir)?.ok_or_else(|| IndexError::Io(std::io::Error::other("fresh index is inconsistent")))
            }
        }
    }

    fn wipe(dir: &Path) -> Result<()> {
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            match std::fs::remove_file(dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        match std::fs::remove_dir_all(dir.join("tantivy")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        Ok(())
    }

    fn try_open(dir: &Path) -> Result<Option<Self>> {
        let db = Connection::open(dir.join("index.db"))?;
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;
        db.execute_batch(DB_SCHEMA)?;
        let version: Option<i64> = db
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get::<_, String>(0))
            .optional()?
            .and_then(|v| v.parse().ok());
        let rows: i64 = db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))?;
        match version {
            None if rows == 0 => {
                db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)", [SCHEMA_VERSION.to_string()])?;
            }
            Some(v) if v == SCHEMA_VERSION => {}
            _ => return Ok(None),
        }

        let tdir: PathBuf = dir.join("tantivy");
        std::fs::create_dir_all(&tdir)?;
        let (schema, fields) = schema();
        let index = match tantivy::Index::open_in_dir(&tdir) {
            Ok(i) => {
                if i.schema() != schema {
                    return Ok(None);
                }
                i
            }
            Err(tantivy::TantivyError::OpenDirectoryError(_)) | Err(tantivy::TantivyError::OpenReadError(_)) => {
                tantivy::Index::create_in_dir(&tdir, schema)?
            }
            Err(e) => return Err(e.into()),
        };
        let writer = index.writer(WRITER_HEAP)?;
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let docs = reader.searcher().num_docs() as i64;
        if docs != rows {
            tracing::warn!(rows, docs, "index parts disagree");
            drop(writer);
            return Ok(None);
        }
        Ok(Some(Self { db: Mutex::new(db), writer: Mutex::new(writer), reader, index, fields }))
    }

    /// Starts a batch of changes for one connection.
    pub fn batch(&self, user_id: i64, connection_id: i64) -> Batch<'_> {
        Batch { index: self, user_id, connection_id, pending: Vec::new(), upserted: 0, deleted: 0 }
    }

    /// What's indexed for a connection, for a sync to compare against.
    pub fn known(&self, connection_id: i64) -> Result<Known> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = db.prepare("SELECT external_id, fingerprint FROM documents WHERE connection_id = ?1")?;
        let known = stmt
            .query_map([connection_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?
            .collect::<rusqlite::Result<Known>>()?;
        Ok(known)
    }

    pub fn count(&self, connection_id: i64) -> Result<u64> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        Ok(db.query_row("SELECT COUNT(*) FROM documents WHERE connection_id = ?1", [connection_id], |r| r.get::<_, i64>(0))?
            as u64)
    }

    /// One item, if `user_id` owns it.
    pub fn doc(&self, user_id: i64, connection_id: i64, external_id: &str) -> Result<Option<DocRow>> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        Ok(db
            .query_row(
                &format!("SELECT {ROW} FROM documents WHERE user_id = ?1 AND connection_id = ?2 AND external_id = ?3"),
                params![user_id, connection_id, external_id],
                row,
            )
            .optional()?)
    }

    /// Drops everything a connection indexed (it was deleted).
    pub fn remove_connection(&self, connection_id: i64) -> Result<u64> {
        let mut writer = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let mut db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        let tx = db.transaction()?;
        let n = tx.execute("DELETE FROM documents WHERE connection_id = ?1", [connection_id])? as u64;
        tx.execute("DELETE FROM sync_state WHERE connection_id = ?1", [connection_id])?;
        writer.delete_term(Term::from_field_i64(self.fields.connection, connection_id));
        writer.commit()?;
        tx.commit()?;
        drop(db);
        self.reader.reload()?;
        Ok(n)
    }

    pub fn sync_state(&self, connection_id: i64) -> Result<SyncState> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        Ok(db
            .query_row("SELECT last_full_at, last_error FROM sync_state WHERE connection_id = ?1", [connection_id], |r| {
                Ok(SyncState { last_full_at: r.get(0)?, last_error: r.get(1)? })
            })
            .optional()?
            .unwrap_or_default())
    }

    /// Records how a sync ended: `Ok` for a full sync that finished, `Err`
    /// for one that failed (keeping the last good time).
    pub fn record_sync(&self, connection_id: i64, outcome: std::result::Result<(), String>) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        match outcome {
            Ok(()) => db.execute(
                "INSERT INTO sync_state (connection_id, last_full_at, last_error) VALUES (?1, ?2, NULL)
                 ON CONFLICT (connection_id) DO UPDATE SET last_full_at = excluded.last_full_at, last_error = NULL",
                params![connection_id, now()],
            )?,
            Err(e) => db.execute(
                "INSERT INTO sync_state (connection_id, last_error) VALUES (?1, ?2)
                 ON CONFLICT (connection_id) DO UPDATE SET last_error = excluded.last_error",
                params![connection_id, e],
            )?,
        };
        Ok(())
    }

    /// Full-text search over one user's items. Matches every word (in the
    /// title, path or text), ranks title matches highest, and treats the
    /// title's words as prefixes so results appear while typing.
    pub fn search(&self, user_id: i64, q: &SearchQuery) -> Result<Vec<Hit>> {
        if q.text.trim().is_empty() || q.limit == 0 {
            return Ok(Vec::new());
        }
        let f = self.fields;
        let mut parser = QueryParser::for_index(&self.index, vec![f.title, f.path, f.body]);
        parser.set_conjunction_by_default();
        parser.set_field_boost(f.title, 3.0);
        parser.set_field_boost(f.path, 1.5);
        parser.set_field_fuzzy(f.title, true, 0, false);
        // Lenient: whatever someone types is a search, never a syntax error.
        let (text_query, _errors) = parser.parse_query_lenient(&q.text);

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, text_query.box_clone()),
            (Occur::Must, Box::new(TermQuery::new(Term::from_field_i64(f.user, user_id), IndexRecordOption::Basic))),
        ];
        if let Some(kind) = &q.kind {
            clauses.push((Occur::Must, Box::new(TermQuery::new(Term::from_field_text(f.kind, kind), IndexRecordOption::Basic))));
        }
        if let Some(c) = q.connection {
            clauses.push((Occur::Must, Box::new(TermQuery::new(Term::from_field_i64(f.connection, c), IndexRecordOption::Basic))));
        }
        let query = BooleanQuery::new(clauses);

        let searcher = self.reader.searcher();
        let top = searcher.search(&query, &TopDocs::with_limit(q.limit).order_by_score())?;
        let snippets = SnippetGenerator::create(&searcher, &*text_query, f.body).ok().map(|mut g| {
            g.set_max_num_chars(200);
            g
        });

        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = db.prepare(&format!("SELECT {ROW} FROM documents WHERE id = ?1 AND user_id = ?2"))?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let Some(id) = doc.get_first(f.doc_id).and_then(|v| v.as_u64()) else { continue };
            // The row check repeats the user filter: belt and braces.
            let Some(row) = stmt.query_row(params![id as i64, user_id], row).optional()? else { continue };
            let snippet = snippets.as_ref().and_then(|g| {
                let s = g.snippet_from_doc(&doc);
                (!s.fragment().is_empty() && !s.highlighted().is_empty())
                    .then(|| (s.fragment().to_owned(), s.highlighted().iter().map(|r| (r.start, r.end)).collect()))
            });
            hits.push(Hit { row, score, snippet });
        }
        Ok(hits)
    }
}

enum Op {
    Upsert(Doc),
    Delete(String),
}

/// Changes for one connection, flushed every [`FLUSH_EVERY`] and on
/// [`Batch::commit`]. Dropping it without committing loses what wasn't
/// flushed yet, which the next sync redoes.
pub struct Batch<'a> {
    index: &'a Index,
    user_id: i64,
    connection_id: i64,
    pending: Vec<Op>,
    pub upserted: u64,
    pub deleted: u64,
}

impl Batch<'_> {
    pub fn commit(mut self) -> Result<()> {
        self.flush()
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let ix = self.index;
        let f = ix.fields;
        let mut writer = ix.writer.lock().unwrap_or_else(|p| p.into_inner());
        let mut db = ix.db.lock().unwrap_or_else(|p| p.into_inner());
        let tx = db.transaction()?;
        let t = now();
        for op in self.pending.drain(..) {
            match op {
                Op::Upsert(d) => {
                    let id: i64 = tx.query_row(
                        "INSERT INTO documents (user_id, connection_id, external_id, kind, title, path, mime, size, mtime, fingerprint, indexed_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                         ON CONFLICT (connection_id, external_id) DO UPDATE SET
                            kind = excluded.kind, title = excluded.title, path = excluded.path, mime = excluded.mime,
                            size = excluded.size, mtime = excluded.mtime, fingerprint = excluded.fingerprint,
                            indexed_at = excluded.indexed_at
                         RETURNING id",
                        params![
                            self.user_id,
                            self.connection_id,
                            d.external_id,
                            d.kind,
                            d.title,
                            d.path,
                            d.mime,
                            d.size.map(|s| s as i64),
                            d.mtime,
                            d.fingerprint,
                            t
                        ],
                        |r| r.get(0),
                    )?;
                    writer.delete_term(Term::from_field_u64(f.doc_id, id as u64));
                    writer.add_document(doc!(
                        f.doc_id => id as u64,
                        f.user => self.user_id,
                        f.connection => self.connection_id,
                        f.kind => d.kind,
                        f.title => d.title,
                        f.path => d.path.unwrap_or_default(),
                        f.body => d.body.unwrap_or_default(),
                    ))?;
                    self.upserted += 1;
                }
                Op::Delete(external_id) => {
                    let id: Option<i64> = tx
                        .query_row(
                            "DELETE FROM documents WHERE connection_id = ?1 AND external_id = ?2 RETURNING id",
                            params![self.connection_id, external_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(id) = id {
                        writer.delete_term(Term::from_field_u64(f.doc_id, id as u64));
                        self.deleted += 1;
                    }
                }
            }
        }
        writer.commit()?;
        tx.commit()?;
        drop(db);
        drop(writer);
        ix.reader.reload()?;
        Ok(())
    }
}

impl IndexSink for Batch<'_> {
    fn upsert(&mut self, doc: Doc) -> atlas_core::Result<()> {
        self.pending.push(Op::Upsert(doc));
        if self.pending.len() >= FLUSH_EVERY {
            self.flush()?;
        }
        Ok(())
    }

    fn delete(&mut self, id: &str) -> atlas_core::Result<()> {
        self.pending.push(Op::Delete(id.to_owned()));
        if self.pending.len() >= FLUSH_EVERY {
            self.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, title: &str, body: &str) -> Doc {
        Doc {
            external_id: id.into(),
            kind: "file".into(),
            title: title.into(),
            path: Some(id.rsplit_once('/').map(|(p, _)| p.to_owned()).unwrap_or_default()),
            mime: Some("text/plain".into()),
            size: Some(body.len() as u64),
            mtime: Some(1_700_000_000),
            fingerprint: Some(format!("{}:1", body.len())),
            body: Some(body.into()),
        }
    }

    fn q(text: &str) -> SearchQuery {
        SearchQuery { text: text.into(), kind: None, connection: None, limit: 20 }
    }

    fn titles(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.row.title.as_str()).collect()
    }

    fn fill(ix: &Index, user: i64, conn: i64, docs: &[Doc]) {
        let mut b = ix.batch(user, conn);
        for d in docs {
            b.upsert(d.clone()).unwrap();
        }
        b.commit().unwrap();
    }

    #[test]
    fn finds_by_title_body_and_stem() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        fill(
            &ix,
            1,
            10,
            &[
                file("Taxes/2025 return.txt", "2025 return.txt", "The refund arrives in March."),
                file("Recipes/soup.md", "soup.md", "Simmer the onions with taxes in mind."),
                file("notes.txt", "notes.txt", "nothing here"),
            ],
        );
        let hits = ix.search(1, &q("refund")).unwrap();
        assert_eq!(titles(&hits), ["2025 return.txt"]);
        let (frag, hl) = hits[0].snippet.clone().unwrap();
        assert_eq!(&frag[hl[0].0..hl[0].1], "refund");

        // "taxes" in a path beats "taxes" in a body; "tax" stems to both.
        let hits = ix.search(1, &q("tax")).unwrap();
        assert_eq!(titles(&hits).len(), 2);
        assert_eq!(hits[0].row.title, "2025 return.txt");

        // Prefix on titles, so results appear while typing.
        assert_eq!(titles(&ix.search(1, &q("sou")).unwrap()), ["soup.md"]);
        // Every word must match.
        assert!(ix.search(1, &q("refund onions")).unwrap().is_empty());
        // Typed syntax isn't an error.
        assert!(ix.search(1, &q("refund AND (")).is_ok());
        assert!(ix.search(1, &q("   ")).unwrap().is_empty());
    }

    #[test]
    fn users_only_find_their_own() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        fill(&ix, 1, 10, &[file("a.txt", "alice.txt", "shared secret word")]);
        fill(&ix, 2, 20, &[file("b.txt", "bob.txt", "shared secret word")]);
        assert_eq!(titles(&ix.search(1, &q("secret")).unwrap()), ["alice.txt"]);
        assert_eq!(titles(&ix.search(2, &q("secret")).unwrap()), ["bob.txt"]);
        assert!(ix.search(3, &q("secret")).unwrap().is_empty());
        assert!(ix.doc(2, 10, "a.txt").unwrap().is_none(), "bob can't fetch alice's row");
        assert!(ix.doc(1, 10, "a.txt").unwrap().is_some());
    }

    #[test]
    fn filters_by_kind_and_connection() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        let mut album = file("album-1", "Beach trip", "");
        album.kind = "album".into();
        fill(&ix, 1, 10, &[file("beach.txt", "beach.txt", "sand")]);
        fill(&ix, 1, 11, &[album]);
        assert_eq!(ix.search(1, &q("beach")).unwrap().len(), 2);
        let mut only = q("beach");
        only.kind = Some("album".into());
        assert_eq!(titles(&ix.search(1, &only).unwrap()), ["Beach trip"]);
        let mut only = q("beach");
        only.connection = Some(10);
        assert_eq!(titles(&ix.search(1, &only).unwrap()), ["beach.txt"]);
    }

    #[test]
    fn upserts_replace_and_deletes_remove() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        fill(&ix, 1, 10, &[file("a.txt", "a.txt", "first draft")]);
        fill(&ix, 1, 10, &[file("a.txt", "a.txt", "second version")]);
        assert!(ix.search(1, &q("draft")).unwrap().is_empty());
        assert_eq!(ix.search(1, &q("version")).unwrap().len(), 1);
        assert_eq!(ix.count(10).unwrap(), 1);
        assert_eq!(ix.known(10).unwrap()["a.txt"].as_deref(), Some("14:1"));

        let mut b = ix.batch(1, 10);
        b.delete("a.txt").unwrap();
        b.delete("never-existed").unwrap();
        b.commit().unwrap();
        assert!(ix.search(1, &q("version")).unwrap().is_empty());
        assert_eq!(ix.count(10).unwrap(), 0);
    }

    #[test]
    fn removing_a_connection_drops_its_items() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        fill(&ix, 1, 10, &[file("a.txt", "a.txt", "word")]);
        fill(&ix, 1, 11, &[file("b.txt", "b.txt", "word")]);
        ix.record_sync(10, Ok(())).unwrap();
        assert_eq!(ix.remove_connection(10).unwrap(), 1);
        assert_eq!(titles(&ix.search(1, &q("word")).unwrap()), ["b.txt"]);
        assert_eq!(ix.sync_state(10).unwrap(), SyncState::default());
    }

    #[test]
    fn large_syncs_flush_as_they_go() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        let mut b = ix.batch(1, 10);
        for i in 0..(FLUSH_EVERY + 5) {
            b.upsert(file(&format!("f{i}.txt"), &format!("f{i}.txt"), "bulk")).unwrap();
        }
        assert_eq!(ix.count(10).unwrap() as usize, FLUSH_EVERY, "flushed before commit");
        b.commit().unwrap();
        assert_eq!(ix.count(10).unwrap() as usize, FLUSH_EVERY + 5);
    }

    #[test]
    fn reopens_with_its_contents_and_rebuilds_when_parts_disagree() {
        let d = tempfile::tempdir().unwrap();
        {
            let ix = Index::open(d.path()).unwrap();
            fill(&ix, 1, 10, &[file("a.txt", "a.txt", "persist")]);
        }
        {
            let ix = Index::open(d.path()).unwrap();
            assert_eq!(ix.search(1, &q("persist")).unwrap().len(), 1);
            // Simulate a crash between the two commits: a row with no text.
            ix.db
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO documents (user_id, connection_id, external_id, kind, title, indexed_at) VALUES (1, 10, 'ghost', 'file', 'ghost', 0)",
                    [],
                )
                .unwrap();
        }
        let ix = Index::open(d.path()).unwrap();
        assert_eq!(ix.count(10).unwrap(), 0, "started over");
        assert!(ix.search(1, &q("persist")).unwrap().is_empty());
    }

    #[test]
    fn sync_state_keeps_the_last_good_time() {
        let d = tempfile::tempdir().unwrap();
        let ix = Index::open(d.path()).unwrap();
        ix.record_sync(10, Ok(())).unwrap();
        let good = ix.sync_state(10).unwrap().last_full_at;
        assert!(good.is_some());
        ix.record_sync(10, Err("disk gone".into())).unwrap();
        let s = ix.sync_state(10).unwrap();
        assert_eq!(s.last_full_at, good);
        assert_eq!(s.last_error.as_deref(), Some("disk gone"));
    }
}
