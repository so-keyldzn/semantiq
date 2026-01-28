//! Chunk operations for IndexStore.

use super::IndexStore;
use crate::schema::ChunkRecord;
use anyhow::{Result, anyhow};
use rusqlite::{params, OptionalExtension};
use semantiq_parser::CodeChunk;
use std::sync::{MutexGuard, PoisonError};
use tracing::{debug, warn};
use rusqlite::Connection;

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
fn parse_embedding_bytes(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        warn!(
            "Invalid embedding bytes length: {} (not divisible by 4)",
            bytes.len()
        );
        return Vec::new();
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let bytes: [u8; 4] = chunk.try_into().expect("chunks_exact guarantees 4 bytes");
            f32::from_le_bytes(bytes)
        })
        .collect()
}

impl IndexStore {
    /// Insert chunks for a file (replaces existing chunks for that file).
    pub fn insert_chunks(&self, file_id: i64, chunks: &[CodeChunk]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            // Delete existing chunks for this file
            conn.execute("DELETE FROM chunks WHERE file_id = ?1", [file_id])?;

            let mut stmt = conn.prepare(
                "INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;

            for chunk in chunks {
                let symbols_json = serde_json::to_string(&chunk.symbols)?;
                stmt.execute(params![
                    file_id,
                    chunk.content,
                    chunk.start_line as i64,
                    chunk.end_line as i64,
                    chunk.start_byte as i64,
                    chunk.end_byte as i64,
                    symbols_json,
                ])?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!("Inserted {} chunks for file_id {}", chunks.len(), file_id);
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Update the embedding for a chunk.
    pub fn update_chunk_embedding(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        self.with_conn(|conn| {
            // Convert f32 slice to bytes for the chunks table
            let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

            // Update the chunks table (for backward compatibility)
            conn.execute(
                "UPDATE chunks SET embedding = ?1 WHERE id = ?2",
                params![embedding_bytes, chunk_id],
            )?;

            // Insert/replace into the vec0 virtual table for vector search
            conn.execute(
                "INSERT OR REPLACE INTO chunks_vec(chunk_id, embedding) VALUES (?1, ?2)",
                params![chunk_id, embedding_bytes],
            )?;

            Ok(())
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
    pub fn get_chunks_by_ids(&self, chunk_ids: &[i64]) -> Result<Vec<ChunkRecord>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }

        self.with_conn(|conn| {
            let placeholders: String = chunk_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, symbols_json, embedding
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let mut stmt = conn.prepare(&query)?;
            let params: Vec<&dyn rusqlite::ToSql> = chunk_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

            let results = stmt
                .query_map(params.as_slice(), |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols = parse_symbols_json(&symbols_json);
                    let embedding_bytes: Option<Vec<u8>> = row.get(8)?;
                    let embedding = embedding_bytes.map(|b| parse_embedding_bytes(&b));

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get chunks that don't have embeddings yet.
    pub fn get_chunks_without_embeddings(&self, limit: usize) -> Result<Vec<ChunkRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, symbols_json
                 FROM chunks WHERE embedding IS NULL
                 LIMIT ?1",
            )?;

            let results = stmt
                .query_map([limit as i64], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols = parse_symbols_json(&symbols_json);

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: None,
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
                "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, symbols_json
                 FROM chunks WHERE file_id = ?1",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols = parse_symbols_json(&symbols_json);

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: None,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all chunks that have embeddings.
    pub fn get_chunks_with_embeddings(&self) -> Result<Vec<(ChunkRecord, Vec<f32>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.file_id, c.content, c.start_line, c.end_line, c.start_byte, c.end_byte, c.symbols_json, c.embedding, f.path
                 FROM chunks c
                 JOIN files f ON c.file_id = f.id
                 WHERE c.embedding IS NOT NULL",
            )?;

            let results = stmt
                .query_map([], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols = parse_symbols_json(&symbols_json);
                    let embedding_bytes: Vec<u8> = row.get(8)?;
                    let embedding = parse_embedding_bytes(&embedding_bytes);

                    let chunk = ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: Some(embedding.clone()),
                    };

                    Ok((chunk, embedding))
                })?
                .filter_map(|r| {
                    r.map_err(|e| warn!("Failed to load chunk with embedding: {}", e)).ok()
                })
                .collect();

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
