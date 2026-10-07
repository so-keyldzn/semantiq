//! Chunk operations for IndexStore.

use super::IndexStore;
use crate::schema::{ChunkRecord, EMBEDDING_DIMENSION};
use anyhow::{Result, anyhow, bail};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use semantiq_parser::CodeChunk;
use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use tracing::{debug, warn};

/// Parse symbols JSON with logging on error.
fn parse_symbols_json(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_else(|e| {
        if !json.is_empty() && json != "[]" {
            warn!("Failed to parse symbols JSON: {} (json: {})", e, json);
        }
        Vec::new()
    })
}

/// Convert embedding bytes to f32 vector with validation.
/// A chunk waiting for its embedding.
#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub id: i64,
    pub file_id: i64,
    pub content: String,
}

/// How many chunks have an embedding, out of all indexed chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingCounts {
    pub embedded: usize,
    pub total: usize,
}

impl EmbeddingCounts {
    /// Chunks still waiting for an embedding.
    pub fn pending(&self) -> usize {
        self.total - self.embedded
    }

    /// Share of chunks with an embedding, rounded down (100 when there is
    /// nothing to embed).
    pub fn percent(&self) -> u8 {
        (self.embedded * 100)
            .checked_div(self.total)
            .map_or(100, |percent| percent as u8)
    }
}

fn parse_embedding_bytes(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        warn!(
            "Invalid embedding bytes length: {} (not divisible by 4)",
            bytes.len()
        );
        return Vec::new();
    }
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

impl IndexStore {
    /// Insert chunks for a file (replaces existing chunks for that file).
    ///
    /// A new chunk whose content is identical to one of the file's previous
    /// chunks keeps that chunk's embedding (and gets its `chunks_vec` row): an
    /// edit then only leaves the changed chunks waiting for phase 2.
    pub fn insert_chunks(&self, file_id: i64, chunks: &[CodeChunk]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<usize> {
            // Embeddings of the previous chunks, keyed by content.
            let mut previous: HashMap<String, Vec<u8>> = HashMap::new();
            {
                let mut stmt = conn.prepare(
                    "SELECT content, embedding FROM chunks
                     WHERE file_id = ?1 AND embedding IS NOT NULL",
                )?;
                let rows = stmt.query_map([file_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?;
                for row in rows {
                    let (content, embedding) = row?;
                    if embedding.len() == EMBEDDING_DIMENSION * 4 {
                        previous.insert(content, embedding);
                    }
                }
            }

            // Purge sqlite-vec rows for this file's old chunks. The vec0 virtual
            // table doesn't honor FK / ON DELETE CASCADE, so we have to do it
            // ourselves — must run BEFORE the chunks DELETE, otherwise the
            // subquery would see no rows. Doing it as a subquery keeps the IDs
            // entirely inside SQLite (no Rust round-trip, no IN clause variable
            // limit to worry about even for very large files).
            conn.execute(
                "DELETE FROM chunks_vec
                 WHERE chunk_id IN (SELECT id FROM chunks WHERE file_id = ?1)",
                [file_id],
            )?;

            // Delete existing chunks for this file
            conn.execute("DELETE FROM chunks WHERE file_id = ?1", [file_id])?;

            let mut stmt = conn.prepare(
                "INSERT INTO chunks (file_id, content, start_line, end_line, symbols_json, embedding)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut vec_stmt =
                conn.prepare("INSERT INTO chunks_vec(chunk_id, embedding) VALUES (?1, ?2)")?;

            let mut reused = 0;
            for chunk in chunks {
                let symbols_json = serde_json::to_string(&chunk.symbols)?;
                let embedding = previous.get(&chunk.content);
                stmt.execute(params![
                    file_id,
                    chunk.content,
                    chunk.start_line as i64,
                    chunk.end_line as i64,
                    symbols_json,
                    embedding,
                ])?;
                if let Some(embedding) = embedding {
                    vec_stmt.execute(params![conn.last_insert_rowid(), embedding])?;
                    reused += 1;
                }
            }
            Ok(reused)
        })();

        match result {
            Ok(reused) => {
                conn.execute("COMMIT", [])?;
                debug!(
                    "Inserted {} chunks for file_id {} ({} embeddings reused)",
                    chunks.len(),
                    file_id,
                    reused
                );
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Update the embedding for a chunk.
    ///
    /// The two writes (the `chunks.embedding` BLOB and the `chunks_vec` virtual
    /// table row) are wrapped in a single `BEGIN IMMEDIATE`/`COMMIT` transaction
    /// so a failure on the second write can never leave the chunk with a stored
    /// embedding but no searchable vector (or vice versa).
    pub fn update_chunk_embedding(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        self.store_chunk_embeddings(&[(chunk_id, embedding)])
            .map(|_| ())
    }

    /// Store the embeddings of several chunks in one transaction and return
    /// how many were written.
    ///
    /// A chunk deleted since its content was read (its file was reindexed while
    /// the vector was computed) is skipped: no `chunks_vec` row is written for
    /// it, so the vector index never gains an orphan. Chunk ids are never
    /// reused (`AUTOINCREMENT`), so a vector cannot land on a newer chunk.
    pub fn store_chunk_embeddings(&self, embeddings: &[(i64, &[f32])]) -> Result<usize> {
        // Reject mis-sized vectors up front with a clear error. The `chunks_vec`
        // vec0 table is declared `float[EMBEDDING_DIMENSION]` and would otherwise
        // fail with an opaque dimension-mismatch error; a wrong length also means
        // the embedding model and schema disagree, which is a bug worth surfacing.
        for (_, embedding) in embeddings {
            if embedding.len() != EMBEDDING_DIMENSION {
                bail!(
                    "embedding length {} does not match expected dimension {}",
                    embedding.len(),
                    EMBEDDING_DIMENSION
                );
            }
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<usize> {
            let mut update = conn.prepare("UPDATE chunks SET embedding = ?1 WHERE id = ?2")?;
            let mut upsert_vec = conn.prepare(
                "INSERT OR REPLACE INTO chunks_vec(chunk_id, embedding) VALUES (?1, ?2)",
            )?;
            let mut written = 0;
            for (chunk_id, embedding) in embeddings {
                // Convert f32 slice to bytes for the chunks table
                let embedding_bytes: Vec<u8> =
                    embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                // The chunks table copy is what marks the chunk as done.
                if update.execute(params![embedding_bytes, chunk_id])? == 0 {
                    continue;
                }
                // The vec0 virtual table serves vector search.
                upsert_vec.execute(params![chunk_id, embedding_bytes])?;
                written += 1;
            }
            Ok(written)
        })();

        match result {
            Ok(written) => {
                conn.execute("COMMIT", [])?;
                Ok(written)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Chunks still waiting for an embedding (phase 2), by increasing id,
    /// starting after `after_id`. Walking by id keeps a full pass linear and
    /// picks up chunks inserted meanwhile (ids only grow).
    pub fn pending_embedding_chunks(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<PendingChunk>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, content FROM chunks
                 WHERE id > ?1 AND embedding IS NULL
                 ORDER BY id
                 LIMIT ?2",
            )?;
            let results = stmt
                .query_map(params![after_id, limit as i64], |row| {
                    Ok(PendingChunk {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(results)
        })
    }

    /// Progress of phase 2: chunks with an embedding, and all chunks.
    pub fn embedding_counts(&self) -> Result<EmbeddingCounts> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*), COUNT(*) - COUNT(embedding) FROM chunks",
                [],
                |row| {
                    let total = row.get::<_, i64>(0)? as usize;
                    let pending = row.get::<_, i64>(1)? as usize;
                    Ok(EmbeddingCounts {
                        embedded: total - pending,
                        total,
                    })
                },
            )
            .map_err(Into::into)
        })
    }

    /// Search for similar chunks using vector similarity (sqlite-vec).
    /// Returns chunk IDs with their distances, ordered by similarity (closest first).
    pub fn search_similar_chunks(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        self.with_conn(|conn| {
            let embedding_bytes: Vec<u8> = query_embedding
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();

            let mut stmt = conn.prepare(
                "SELECT chunk_id, distance
                 FROM chunks_vec
                 WHERE embedding MATCH ?1
                 ORDER BY distance
                 LIMIT ?2",
            )?;

            let results = stmt
                .query_map(params![embedding_bytes, limit as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get chunk records by IDs (useful after vector search).
    ///
    /// Results come back in the order of `chunk_ids` (missing IDs are skipped),
    /// so callers can pass a KNN result list and keep its nearest-first order.
    pub fn get_chunks_by_ids(&self, chunk_ids: &[i64]) -> Result<Vec<ChunkRecord>> {
        let mut by_id: std::collections::HashMap<i64, ChunkRecord> = self
            .get_chunks_by_ids_unordered(chunk_ids)?
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
        Ok(chunk_ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Fetch chunk records by IDs in rowid order.
    ///
    /// If more than 900 IDs are provided, the query is split into batches
    /// to stay within SQLite's `SQLITE_MAX_VARIABLE_NUMBER` limit (default 999).
    fn get_chunks_by_ids_unordered(&self, chunk_ids: &[i64]) -> Result<Vec<ChunkRecord>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }

        // SQLite default SQLITE_MAX_VARIABLE_NUMBER is 999.
        // Process in batches to avoid exceeding the limit.
        const BATCH_SIZE: usize = 900;
        if chunk_ids.len() > BATCH_SIZE {
            let mut all_results = Vec::new();
            for batch in chunk_ids.chunks(BATCH_SIZE) {
                all_results.extend(self.get_chunks_by_ids_unordered(batch)?);
            }
            return Ok(all_results);
        }

        self.with_conn(|conn| {
            let placeholders: String = chunk_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT id, file_id, content, start_line, end_line, symbols_json, embedding
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let mut stmt = conn.prepare(&query)?;
            let params: Vec<&dyn rusqlite::ToSql> = chunk_ids
                .iter()
                .map(|id| id as &dyn rusqlite::ToSql)
                .collect();

            let results = stmt
                .query_map(params.as_slice(), |row| {
                    let symbols_json: String = row.get(5)?;
                    let symbols = parse_symbols_json(&symbols_json);
                    let embedding_bytes: Option<Vec<u8>> = row.get(6)?;
                    let embedding = embedding_bytes.map(|b| parse_embedding_bytes(&b));

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        symbols,
                        embedding,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all chunks for a file.
    pub fn get_chunks_by_file(&self, file_id: i64) -> Result<Vec<ChunkRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, content, start_line, end_line, symbols_json
                 FROM chunks WHERE file_id = ?1",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    let symbols_json: String = row.get(5)?;
                    let symbols = parse_symbols_json(&symbols_json);

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        symbols,
                        embedding: None,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get the file path for a chunk's file.
    pub fn get_chunk_file_path(&self, file_id: i64) -> Result<Option<String>> {
        self.get_file_path_by_id(file_id)
    }

    /// Get the language for a chunk by looking up its file.
    pub fn get_chunk_language(&self, chunk_id: i64) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row(
                    "SELECT f.language FROM chunks c
                     JOIN files f ON c.file_id = f.id
                     WHERE c.id = ?1",
                    [chunk_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(result)
        })
    }
}
