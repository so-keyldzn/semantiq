//! Distance observation operations for ML calibration.

use super::IndexStore;
use anyhow::{Result, anyhow};
use rusqlite::params;
use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use tracing::{debug, info};
use rusqlite::Connection;

impl IndexStore {
    /// Insert a distance observation for threshold calibration.
    ///
    /// Uses INSERT OR IGNORE to handle the UNIQUE constraint on (query_hash, language).
    pub fn insert_distance_observation(
        &self,
        language: &str,
        distance: f32,
        query_hash: u64,
        timestamp: i64,
    ) -> Result<bool> {
        self.with_conn(|conn| {
            let rows = conn.execute(
                "INSERT OR IGNORE INTO distance_observations (language, distance, query_hash, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
                params![language, distance, query_hash as i64, timestamp],
            )?;
            Ok(rows > 0)
        })
    }

    /// Insert multiple distance observations in a batch.
    pub fn insert_distance_observations_batch(
        &self,
        observations: &[(String, f32, u64, i64)],
    ) -> Result<usize> {
        if observations.is_empty() {
            return Ok(0);
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<usize> {
            let mut stmt = conn.prepare(
                "INSERT OR IGNORE INTO distance_observations (language, distance, query_hash, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;

            let mut inserted = 0;
            for (language, distance, query_hash, timestamp) in observations {
                let rows =
                    stmt.execute(params![language, distance, *query_hash as i64, timestamp])?;
                inserted += rows;
            }
            Ok(inserted)
        })();

        match result {
            Ok(inserted) => {
                conn.execute("COMMIT", [])?;
                debug!("Inserted {} distance observations", inserted);
                Ok(inserted)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Get distance observations for a specific language.
    pub fn get_distance_observations(&self, language: &str) -> Result<Vec<f32>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT distance FROM distance_observations WHERE language = ?1")?;

            let results = stmt
                .query_map([language], |row| row.get(0))?
                .collect::<Result<Vec<f32>, _>>()?;

            Ok(results)
        })
    }

    /// Get all distance observations grouped by language.
    pub fn get_all_distance_observations(&self) -> Result<HashMap<String, Vec<f32>>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT language, distance FROM distance_observations ORDER BY language",
            )?;

            let mut results: HashMap<String, Vec<f32>> = HashMap::new();

            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
            })?;

            for row in rows {
                let (language, distance) = row?;
                results.entry(language).or_default().push(distance);
            }

            Ok(results)
        })
    }

    /// Get the count of observations per language.
    pub fn get_observation_counts(&self) -> Result<HashMap<String, usize>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT language, COUNT(*) FROM distance_observations GROUP BY language",
            )?;

            let results = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
                })?
                .collect::<Result<HashMap<_, _>, _>>()?;

            Ok(results)
        })
    }

    /// Delete old distance observations.
    ///
    /// Returns the number of observations deleted.
    pub fn cleanup_old_observations(&self, max_age_secs: i64) -> Result<usize> {
        self.with_conn(|conn| {
            let cutoff = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
                - max_age_secs;

            let rows = conn.execute(
                "DELETE FROM distance_observations WHERE timestamp < ?1",
                [cutoff],
            )?;

            if rows > 0 {
                info!("Cleaned up {} old distance observations", rows);
            }

            Ok(rows)
        })
    }
}
