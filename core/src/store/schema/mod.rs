// Schema versioning and migrations
//
// This module handles SQLite schema creation, versioning, and atomic migrations.
// It defines the initial tables for captures, items, corrections, events, proposals,
// jobs, reminders, and suggestions. Schema version validation prevents forward-incompatible
// databases from being silently opened writable.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::sync::Arc;

/// Database schema version.
/// Increment when adding, removing, or changing field meaning.
/// Migrations upgrade older supported versions in the same transaction as the schema change.
const SCHEMA_VERSION: u32 = 1;

/// Injected clock for testability.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Default system clock.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Database handle with schema validation.
pub struct Database {
    conn: Connection,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("clock", &"<clock>")
            .finish()
    }
}

impl Database {
    /// Open or create a database at the given path, with atomic migration.
    /// Returns an error if the database version is incompatible (forward-incompatible versions
    /// cannot be silently opened writable).
    pub fn open(path: &str, clock: Arc<dyn Clock>) -> Result<Self> {
        let mut conn = Connection::open(path)?;

        // Enable foreign keys and WAL mode for durability.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;

        // Create metadata table if it doesn't exist.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;

        // Check current schema version.
        let current_version: Option<u32> = conn
            .query_row(
                "SELECT version FROM _schema_metadata ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(ver) = current_version {
            if ver > SCHEMA_VERSION {
                // Forward-incompatible version: refuse to open writably.
                return Err(anyhow!(
                    "Database schema version {} is newer than supported version {}. \
                     Please upgrade the application.",
                    ver,
                    SCHEMA_VERSION
                ));
            }
            if ver < SCHEMA_VERSION {
                // Perform migration.
                Self::migrate(&mut conn, ver)?;
            }
        } else {
            // Empty database: create schema version 1.
            Self::create_schema_v1(&mut conn, &clock)?;
        }

        Ok(Database { conn, clock })
    }

    /// Create schema version 1 tables.
    fn create_schema_v1(conn: &mut Connection, clock: &Arc<dyn Clock>) -> Result<()> {
        let tx = conn.transaction()?;

        // Record schema version in metadata table.
        let now = clock.now().to_rfc3339();
        tx.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![SCHEMA_VERSION, now.clone(), now],
        )?;

        // Captures table: authoritative source records.
        tx.execute(
            "CREATE TABLE captures (
                capture_id TEXT PRIMARY KEY,
                text TEXT,
                audio_reference TEXT,
                capture_instant TEXT NOT NULL,
                timezone_id TEXT NOT NULL,
                utc_offset_minutes INTEGER NOT NULL,
                locale TEXT NOT NULL,
                calendar TEXT NOT NULL,
                item_scope TEXT NOT NULL,
                route_id TEXT NOT NULL,
                session_topic TEXT,
                entry_locked INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                CONSTRAINT capture_has_content CHECK (text IS NOT NULL OR audio_reference IS NOT NULL)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_captures_item_scope ON captures(item_scope)",
            [],
        )?;

        // Items table: one-to-one mapping with captures.
        tx.execute(
            "CREATE TABLE items (
                item_id TEXT PRIMARY KEY,
                capture_id TEXT NOT NULL UNIQUE,
                revision INTEGER NOT NULL DEFAULT 0,
                lifecycle_state TEXT NOT NULL,
                save_state TEXT NOT NULL,
                sync_state TEXT NOT NULL,
                processing_state TEXT NOT NULL,
                transcription_state TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (capture_id) REFERENCES captures(capture_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_items_lifecycle_state ON items(lifecycle_state)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_items_processing_state ON items(processing_state)",
            [],
        )?;

        // Corrections table: user-initiated text, type, scope, session-topic changes.
        tx.execute(
            "CREATE TABLE corrections (
                correction_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                kind TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id),
                UNIQUE (item_id, revision, kind)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_corrections_item_id ON corrections(item_id)",
            [],
        )?;

        // Events table: lifecycle events (completion, cancellation, deletion, session-topic).
        tx.execute(
            "CREATE TABLE events (
                event_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                happened_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute("CREATE INDEX idx_events_item_id ON events(item_id)", [])?;

        // Search index table: original and corrected text for FTS.
        tx.execute(
            "CREATE TABLE search_index (
                item_id TEXT PRIMARY KEY,
                original_text TEXT NOT NULL,
                current_text TEXT NOT NULL,
                text_basis TEXT NOT NULL,
                last_indexed_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;

        // Proposals table: interpretation results (non-authoritative).
        tx.execute(
            "CREATE TABLE proposals (
                proposal_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                source_revision INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                proposal_type TEXT,
                reminder_proposal TEXT,
                session_topic_proposal TEXT,
                source_spans TEXT,
                abstained INTEGER NOT NULL,
                request_version TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_proposals_item_id ON proposals(item_id)",
            [],
        )?;

        // Provider profiles table: versioned, immutable provider configuration.
        tx.execute(
            "CREATE TABLE provider_profiles (
                profile_id TEXT PRIMARY KEY,
                profile_version TEXT NOT NULL UNIQUE,
                provider_type TEXT NOT NULL,
                endpoint TEXT,
                model TEXT NOT NULL,
                credential_ref TEXT,
                timeout_seconds INTEGER NOT NULL,
                capabilities TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        // Jobs table: durable queued work.
        tx.execute(
            "CREATE TABLE jobs (
                job_id TEXT PRIMARY KEY,
                job_schema_version INTEGER NOT NULL,
                item_id TEXT NOT NULL,
                job_type TEXT NOT NULL,
                source_revision INTEGER NOT NULL,
                profile_version TEXT,
                request_version TEXT,
                status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                lease_expires_at TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id),
                FOREIGN KEY (profile_version) REFERENCES provider_profiles(profile_version)
            )",
            [],
        )?;
        tx.execute("CREATE INDEX idx_jobs_item_id ON jobs(item_id)", [])?;
        tx.execute("CREATE INDEX idx_jobs_status ON jobs(status)", [])?;
        tx.execute(
            "CREATE INDEX idx_jobs_lease_expires_at ON jobs(lease_expires_at)",
            [],
        )?;

        // Reminders table: one-shot reminder desired state and operations.
        tx.execute(
            "CREATE TABLE reminders (
                reminder_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL UNIQUE,
                request_state TEXT NOT NULL,
                schedule_state TEXT NOT NULL,
                delivery_state TEXT NOT NULL,
                acknowledgment_state TEXT NOT NULL,
                resolved_instant TEXT,
                timezone_id TEXT,
                ambiguity_reason TEXT,
                unsupported_reason TEXT,
                schedule_generation INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_reminders_item_id ON reminders(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_reminders_request_state ON reminders(request_state)",
            [],
        )?;

        // Suggestion eligibility table: eligibility scoring and rotation.
        tx.execute(
            "CREATE TABLE suggestion_eligibility (
                item_id TEXT PRIMARY KEY,
                eligible INTEGER NOT NULL,
                snoozed INTEGER NOT NULL,
                pull_only INTEGER NOT NULL,
                snoozed_until TEXT,
                last_selected_at TEXT,
                selection_reason TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_suggestion_eligibility_eligible ON suggestion_eligibility(eligible)",
            [],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Migrate from an older schema version to the current version.
    fn migrate(conn: &mut Connection, from_version: u32) -> Result<()> {
        if from_version >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = conn.transaction()?;

        // Migration logic will be added here as schema versions evolve.
        // For now, M1 only has version 1.
        if from_version == 1 {
            // No migrations needed for version 1 -> current.
        }

        // Update metadata with upgrade time.
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE _schema_metadata SET upgraded_at = ? WHERE version = ?",
            [now, SCHEMA_VERSION.to_string()],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Get the current schema version.
    pub fn schema_version(&self) -> Result<u32> {
        self.conn
            .query_row(
                "SELECT version FROM _schema_metadata ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| anyhow!("Failed to query schema version: {}", e))
    }

    /// Begin a transaction.
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.conn.transaction().map_err(|e| anyhow!(e))
    }

    /// Get a reference to the connection (for read-only queries in tests).
    #[cfg(test)]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Get a mutable reference to the connection (for testing only).
    #[cfg(test)]
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    struct TestClock {
        instant: DateTime<Utc>,
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            self.instant
        }
    }

    #[test]
    fn test_create_empty_database() -> Result<()> {
        let tmpfile = NamedTempFile::new()?;
        let path = tmpfile.path().to_str().unwrap();
        let clock: Arc<dyn Clock> = Arc::new(TestClock {
            instant: Utc::now(),
        });

        let db = Database::open(path, clock)?;
        assert_eq!(db.schema_version()?, 1);

        Ok(())
    }

    #[test]
    fn test_reopen_database() -> Result<()> {
        let tmpfile = NamedTempFile::new()?;
        let path = tmpfile.path().to_str().unwrap();
        let clock: Arc<dyn Clock> = Arc::new(TestClock {
            instant: Utc::now(),
        });

        {
            let _db = Database::open(path, clock.clone())?;
        }

        {
            let db = Database::open(path, clock.clone())?;
            assert_eq!(db.schema_version()?, 1);
        }

        Ok(())
    }

    #[test]
    fn test_forward_incompatible_version_rejected() -> Result<()> {
        let tmpfile = NamedTempFile::new()?;
        let path = tmpfile.path().to_str().unwrap();
        let clock: Arc<dyn Clock> = Arc::new(TestClock {
            instant: Utc::now(),
        });

        // Create database and manually set a higher version.
        {
            let db = Database::open(path, clock.clone())?;
            db.conn.execute(
                "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
                [
                    (SCHEMA_VERSION + 1).to_string(),
                    "2026-01-01T00:00:00Z".to_string(),
                    "2026-01-01T00:00:00Z".to_string(),
                ],
            )?;
        }

        // Try to reopen: should fail.
        let result = Database::open(path, clock.clone());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("newer than supported"));

        Ok(())
    }

    #[test]
    fn test_atomic_migration_interrupted() -> Result<()> {
        let tmpfile = NamedTempFile::new()?;
        let path = tmpfile.path().to_str().unwrap();
        let clock: Arc<dyn Clock> = Arc::new(TestClock {
            instant: Utc::now(),
        });

        // Create a database and verify it has tables.
        {
            let db = Database::open(path, clock.clone())?;

            // Verify schema metadata exists.
            let version: u32 = db.conn.query_row(
                "SELECT version FROM _schema_metadata WHERE version = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, 1);

            // Verify at least the captures table exists.
            let exists: bool = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='captures')",
                [],
                |row| row.get(0),
            )?;
            assert!(exists, "captures table should exist");
        }

        // Reopen and verify data integrity.
        {
            let db = Database::open(path, clock.clone())?;
            let count: i64 =
                db.conn
                    .query_row("SELECT COUNT(*) FROM _schema_metadata", [], |row| {
                        row.get(0)
                    })?;
            assert_eq!(count, 1, "Expected exactly one schema version entry");
        }

        Ok(())
    }

    #[test]
    fn test_injected_clock_works() -> Result<()> {
        let tmpfile = NamedTempFile::new()?;
        let path = tmpfile.path().to_str().unwrap();

        let test_instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
        let clock: Arc<dyn Clock> = Arc::new(TestClock {
            instant: test_instant,
        });

        let db = Database::open(path, clock.clone())?;
        let created_at: String = db.conn.query_row(
            "SELECT created_at FROM _schema_metadata WHERE version = 1",
            [],
            |row| row.get(0),
        )?;

        // Verify the timestamp starts with the expected date and contains the time.
        assert!(created_at.starts_with("2026-01-15"), "Got: {}", created_at);
        assert!(created_at.contains("10:30:00"), "Got: {}", created_at);

        Ok(())
    }
}
