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

        // Check schema version FIRST, on a read-only connection / transaction,
        // before modifying the database.
        Self::check_version(&conn)?;

        // Enable foreign keys for constraint enforcement.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        // Create metadata table if it doesn't exist.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;

        // Query current schema version.
        let current_version: Option<u32> = conn
            .query_row(
                "SELECT version FROM _schema_metadata ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(ver) = current_version {
            if ver < SCHEMA_VERSION {
                // Perform migration.
                Self::migrate(&mut conn, ver, &clock)?;
            }
        } else {
            // Empty database: create schema version 1.
            Self::create_schema_v1(&mut conn, &clock)?;
        }

        Ok(Database { conn, clock })
    }

    /// Check schema version on a read-only connection/transaction.
    /// Prevents forward-incompatible databases from being modified.
    fn check_version(conn: &Connection) -> Result<()> {
        // Read-only query to check if metadata exists and what version is stored.
        let current_version: Option<u32> = conn
            .query_row(
                "SELECT version FROM _schema_metadata ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        if let Some(ver) = current_version {
            if ver > SCHEMA_VERSION {
                return Err(anyhow!(
                    "Database schema version {} is newer than supported version {}. \
                     Please upgrade the application.",
                    ver,
                    SCHEMA_VERSION
                ));
            }
        }

        Ok(())
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
                item_type TEXT,
                current_scope TEXT,
                current_session_topic TEXT,
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

        // Proposals table: interpretation results (non-authoritative).
        // Includes capture_id and text_basis as required by F01 contract.
        tx.execute(
            "CREATE TABLE proposals (
                proposal_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                capture_id TEXT NOT NULL,
                source_revision INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                text_basis_kind TEXT NOT NULL,
                text_basis_id TEXT,
                applied_state TEXT NOT NULL,
                proposal_type TEXT,
                reminder_proposal TEXT,
                session_topic_proposal TEXT,
                source_spans TEXT,
                abstained INTEGER NOT NULL,
                request_version TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id),
                FOREIGN KEY (capture_id) REFERENCES captures(capture_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_proposals_item_id ON proposals(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_proposals_capture_id ON proposals(capture_id)",
            [],
        )?;

        // Search index table: full-text search on original and corrected text.
        tx.execute(
            "CREATE VIRTUAL TABLE search_index USING fts5(
                item_id UNINDEXED,
                original_text,
                current_text,
                text_basis
            )",
            [],
        )?;

        // Provider profiles table: versioned, immutable provider configuration.
        // Note: No FK to jobs; profile can be deleted independently.
        // Jobs record the profile_version and fail visibly if it's no longer available.
        tx.execute(
            "CREATE TABLE provider_profiles (
                profile_id TEXT PRIMARY KEY,
                profile_version TEXT NOT NULL UNIQUE,
                provider_type TEXT NOT NULL,
                endpoint TEXT,
                model TEXT NOT NULL,
                credential_ref TEXT,
                timeout_seconds INTEGER NOT NULL,
                retry_policy TEXT NOT NULL,
                authorized_destinations TEXT NOT NULL,
                capabilities TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        // Capabilities metadata table: per-capability support status and limits.
        tx.execute(
            "CREATE TABLE capabilities (
                capability_id TEXT PRIMARY KEY,
                profile_version TEXT NOT NULL,
                capability_name TEXT NOT NULL,
                support_state TEXT NOT NULL,
                evidence_reference TEXT,
                input_size_limit INTEGER,
                structured_output_supported INTEGER,
                created_at TEXT NOT NULL,
                FOREIGN KEY (profile_version) REFERENCES provider_profiles(profile_version),
                UNIQUE (profile_version, capability_name)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_capabilities_profile_version ON capabilities(profile_version)",
            [],
        )?;

        // Jobs table: durable queued work.
        // Includes failure_reason and next_attempt_at as required by F01 contract.
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
                failure_reason TEXT,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT,
                lease_expires_at TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute("CREATE INDEX idx_jobs_item_id ON jobs(item_id)", [])?;
        tx.execute("CREATE INDEX idx_jobs_status ON jobs(status)", [])?;
        tx.execute(
            "CREATE INDEX idx_jobs_lease_expires_at ON jobs(lease_expires_at)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_jobs_next_attempt_at ON jobs(next_attempt_at)",
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

        // Deletion work table: tracks cleanup work for deleted items.
        tx.execute(
            "CREATE TABLE deletion_work (
                deletion_work_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                work_type TEXT NOT NULL,
                status TEXT NOT NULL,
                attempted_at TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_deletion_work_item_id ON deletion_work(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX idx_deletion_work_status ON deletion_work(status)",
            [],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Migrate from an older schema version to the current version.
    /// Each migration step runs atomically with its version record update.
    fn migrate(conn: &mut Connection, from_version: u32, clock: &Arc<dyn Clock>) -> Result<()> {
        if from_version >= SCHEMA_VERSION {
            return Ok(());
        }

        // Ordered migration steps, each running atomically with version record.
        if from_version < 1 {
            Self::migrate_to_v1(conn, clock)?;
        }

        Ok(())
    }

    /// Migration step: v0 -> v1. Records version 1 for databases with existing v1 tables.
    /// This is used when upgrading from a database that exists but has no version record.
    fn migrate_to_v1(conn: &mut Connection, clock: &Arc<dyn Clock>) -> Result<()> {
        let tx = conn.transaction()?;

        // Metadata table should already exist from create_schema_v1, but handle if missing.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;

        // Record version 1 with upgrade timestamp (or update if already exists).
        let now = clock.now().to_rfc3339();
        // First check if a record exists, to decide whether to insert or update.
        let existing: Option<u32> = tx
            .query_row(
                "SELECT version FROM _schema_metadata WHERE version = ?",
                rusqlite::params![SCHEMA_VERSION],
                |row| row.get(0),
            )
            .optional()?;

        if existing.is_none() {
            tx.execute(
                "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
                rusqlite::params![SCHEMA_VERSION, now.clone(), now],
            )?;
        } else {
            tx.execute(
                "UPDATE _schema_metadata SET upgraded_at = ? WHERE version = ?",
                rusqlite::params![now, SCHEMA_VERSION],
            )?;
        }

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

    struct TestClock {
        instant: DateTime<Utc>,
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            self.instant
        }
    }

    fn make_test_db(path: &str, instant: DateTime<Utc>) -> Result<Database> {
        let clock: Arc<dyn Clock> = Arc::new(TestClock { instant });
        Database::open(path, clock)
    }

    #[test]
    fn test_create_empty_database() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_create_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
        let db = make_test_db(&path, instant)?;
        assert_eq!(db.schema_version()?, 1);

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_reopen_database() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_reopen_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        {
            let _db = make_test_db(&path, instant)?;
        }

        {
            let db = make_test_db(&path, instant)?;
            assert_eq!(db.schema_version()?, 1);
        }

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_forward_incompatible_version_rejected() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_forward_incomp_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        // Create database and manually set a higher version.
        {
            let db = make_test_db(&path, instant)?;
            db.conn.execute(
                "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
                rusqlite::params![
                    SCHEMA_VERSION + 1,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z"
                ],
            )?;
        }

        // Try to reopen: should fail.
        let result = make_test_db(&path, instant);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("newer than supported"));

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_forward_incompatible_database_not_modified() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_forward_unmodified_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        // Create a v1 database.
        {
            let _db = make_test_db(&path, instant)?;
        }

        // Manually insert a v2 record.
        {
            let conn = Connection::open(&path)?;
            conn.execute(
                "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
                rusqlite::params![
                    SCHEMA_VERSION + 1,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z"
                ],
            )?;
        }
        let size_before = std::fs::metadata(&path)?.len();

        // Try to open: should fail without modifying the file.
        let _ = make_test_db(&path, instant);
        let size_after = std::fs::metadata(&path)?.len();

        // File size should not have changed (no WAL or new tables created).
        assert_eq!(
            size_before, size_after,
            "Database file was modified during version check"
        );

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_atomic_migration_interrupted() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_atomic_interrupt_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        // Create initial v1 database and insert a test source record.
        {
            let db = make_test_db(&path, instant)?;
            db.conn.execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params!["test-capture-1", "test text", "2026-01-15T10:30:00Z", "UTC", 0, "en", "gregorian", "personal", "route-1", 0, "2026-01-15T10:30:00Z"],
            )?;
        }

        // Verify the capture record survives reopening.
        {
            let db = make_test_db(&path, instant)?;
            let count: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM captures WHERE capture_id = 'test-capture-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "Source capture should survive migration");
        }

        // Simulate interrupted migration by setting version to 0
        // and attempting to reopen (this triggers migrate_to_v1).
        {
            let conn = Connection::open(&path)?;
            conn.execute("DELETE FROM _schema_metadata", [])?;
            conn.execute(
                "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
                rusqlite::params![0, "2026-01-15T10:00:00Z", "2026-01-15T10:00:00Z"],
            )?;
        }

        // Reopen should recover and recreate version 1 record atomically.
        {
            let db = make_test_db(&path, instant)?;
            let version = db.schema_version()?;
            assert_eq!(version, 1, "Schema version should be restored");

            // Source record should still be intact.
            let count: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM captures WHERE capture_id = 'test-capture-1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                count, 1,
                "Source capture should survive interrupted migration"
            );
        }

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_injected_clock_used_in_schema_creation() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_clock_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let test_instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        let db = make_test_db(&path, test_instant)?;
        let created_at: String = db.conn.query_row(
            "SELECT created_at FROM _schema_metadata WHERE version = 1",
            [],
            |row| row.get(0),
        )?;

        // Verify the timestamp uses the injected clock.
        assert!(created_at.starts_with("2026-01-15"), "Got: {}", created_at);
        assert!(created_at.contains("10:30:00"), "Got: {}", created_at);

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_all_required_tables_exist() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_tables_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        let db = make_test_db(&path, instant)?;

        let expected_tables = vec![
            "captures",
            "items",
            "corrections",
            "events",
            "proposals",
            "search_index",
            "provider_profiles",
            "capabilities",
            "jobs",
            "reminders",
            "suggestion_eligibility",
            "deletion_work",
        ];

        for table_name in expected_tables {
            let exists: bool = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
                rusqlite::params![table_name],
                |row| row.get(0),
            )?;
            assert!(exists, "Table {} should exist", table_name);
        }

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_proposals_have_required_fields() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_proposal_fields_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        let db = make_test_db(&path, instant)?;

        // Check that proposals table has the required columns.
        let mut stmt = db.conn.prepare("PRAGMA table_info(proposals)")?;

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;

        assert!(
            columns.contains(&"capture_id".to_string()),
            "capture_id should exist"
        );
        assert!(
            columns.contains(&"text_basis_kind".to_string()),
            "text_basis_kind should exist"
        );
        assert!(
            columns.contains(&"text_basis_id".to_string()),
            "text_basis_id should exist"
        );
        assert!(
            columns.contains(&"applied_state".to_string()),
            "applied_state should exist"
        );

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_jobs_have_required_fields() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_jobs_fields_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        let db = make_test_db(&path, instant)?;

        // Check that jobs table has the required columns.
        let mut stmt = db.conn.prepare("PRAGMA table_info(jobs)")?;

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;

        assert!(
            columns.contains(&"failure_reason".to_string()),
            "failure_reason should exist"
        );
        assert!(
            columns.contains(&"next_attempt_at".to_string()),
            "next_attempt_at should exist"
        );

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn test_items_have_required_fields() -> Result<()> {
        let tmpdir = std::env::temp_dir();
        let path = format!(
            "{}/test_items_fields_{}.db",
            tmpdir.display(),
            uuid::Uuid::new_v4()
        );
        let _ = std::fs::remove_file(&path);

        let instant =
            DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

        let db = make_test_db(&path, instant)?;

        // Check that items table has the required columns.
        let mut stmt = db.conn.prepare("PRAGMA table_info(items)")?;

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;

        assert!(
            columns.contains(&"item_type".to_string()),
            "item_type should exist"
        );
        assert!(
            columns.contains(&"current_scope".to_string()),
            "current_scope should exist"
        );
        assert!(
            columns.contains(&"current_session_topic".to_string()),
            "current_session_topic should exist"
        );

        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}
