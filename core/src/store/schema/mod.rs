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
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish()
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

        // Query current schema version (metadata table may not exist yet).
        let current_version: Option<u32> = Self::query_version(&conn)?;

        if let Some(ver) = current_version {
            if ver < SCHEMA_VERSION {
                // Perform migration.
                Self::migrate(&mut conn, ver, &clock)?;
            }
        } else {
            // Empty database: create schema version 1.
            Self::create_schema_v1(&mut conn, &clock)?;
        }

        Ok(Database { conn })
    }

    /// Query the current schema version, returning None if metadata table doesn't exist.
    fn query_version(conn: &Connection) -> Result<Option<u32>> {
        // Check if metadata table exists in sqlite_master.
        let table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_schema_metadata')",
                [],
                |row| row.get(0),
            )?;

        if !table_exists {
            return Ok(None);
        }

        // Read version from metadata.
        let version = conn
            .query_row(
                "SELECT version FROM _schema_metadata ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        Ok(version)
    }

    /// Check schema version on a read-only connection/transaction.
    /// Prevents forward-incompatible databases from being modified.
    fn check_version(conn: &Connection) -> Result<()> {
        let current_version = Self::query_version(conn)?;

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

        // Create metadata table inside the transaction for atomicity.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;

        // Record schema version in metadata table.
        let now = clock.now().to_rfc3339();
        tx.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![SCHEMA_VERSION, now.clone(), now],
        )?;

        // Create all v1 tables.
        Self::create_tables_v1(&tx)?;

        tx.commit()?;
        Ok(())
    }

    /// Create or upgrade to v1 schema tables within a transaction.
    fn create_tables_v1(tx: &Transaction) -> Result<()> {
        // Captures table: authoritative source records.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS captures (
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
            "CREATE INDEX IF NOT EXISTS idx_captures_item_scope ON captures(item_scope)",
            [],
        )?;

        // Items table: one-to-one mapping with captures.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS items (
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
            "CREATE INDEX IF NOT EXISTS idx_items_lifecycle_state ON items(lifecycle_state)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_items_processing_state ON items(processing_state)",
            [],
        )?;

        // Corrections table: user-initiated text, type, scope, session-topic changes.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS corrections (
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
            "CREATE INDEX IF NOT EXISTS idx_corrections_item_id ON corrections(item_id)",
            [],
        )?;

        // Events table: lifecycle events (completion, cancellation, deletion, session-topic).
        tx.execute(
            "CREATE TABLE IF NOT EXISTS events (
                event_id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                happened_at TEXT NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(item_id)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_item_id ON events(item_id)",
            [],
        )?;

        // Routes table: user-configured privacy and processing policy.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS routes (
                route_id TEXT PRIMARY KEY,
                route_name TEXT NOT NULL,
                scope TEXT NOT NULL,
                processing_destinations TEXT NOT NULL,
                preview_safe INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_routes_scope ON routes(scope)",
            [],
        )?;

        // Route authorizations table: per-capability authorization for routes.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS route_authorizations (
                auth_id TEXT PRIMARY KEY,
                route_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                authorized_destinations TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (route_id) REFERENCES routes(route_id),
                UNIQUE (route_id, capability)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_route_authorizations_route_id ON route_authorizations(route_id)",
            [],
        )?;

        // Proposals table: interpretation results (non-authoritative).
        // Includes capture_id and text_basis as required by F01 contract.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS proposals (
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
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_proposals_applied_per_revision ON proposals(item_id, source_revision) WHERE applied_state = 'applied'",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_proposals_item_id ON proposals(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_proposals_capture_id ON proposals(capture_id)",
            [],
        )?;

        // Search index table: full-text search on original and corrected text.
        tx.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
                item_id UNINDEXED,
                original_text,
                current_text,
                text_basis
            )",
            [],
        )?;

        // Provider profiles table: versioned, immutable provider configuration.
        // Multiple versions can coexist per profile_id; jobs pin to a specific version.
        // profile_version is the immutable versioning key; profile_id groups related versions.
        // Note: No FK to jobs; profile can be deleted independently.
        // Jobs record the profile_version and fail visibly if it's no longer available.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS provider_profiles (
                profile_version TEXT PRIMARY KEY,
                profile_id TEXT NOT NULL,
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
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_provider_profiles_profile_id ON provider_profiles(profile_id)",
            [],
        )?;

        // Capabilities metadata table: per-capability support status and limits.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS capabilities (
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
            "CREATE INDEX IF NOT EXISTS idx_capabilities_profile_version ON capabilities(profile_version)",
            [],
        )?;

        // Jobs table: durable queued work.
        // Includes failure_reason and next_attempt_at as required by F01 contract.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS jobs (
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
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_item_id ON jobs(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_lease_expires_at ON jobs(lease_expires_at)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_jobs_next_attempt_at ON jobs(next_attempt_at)",
            [],
        )?;

        // Reminders table: one-shot reminder desired state and operations.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS reminders (
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
            "CREATE INDEX IF NOT EXISTS idx_reminders_item_id ON reminders(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminders_request_state ON reminders(request_state)",
            [],
        )?;

        // Reminder operations table: durable record of scheduling/cancellation operations.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS reminder_operations (
                operation_id TEXT PRIMARY KEY,
                reminder_id TEXT NOT NULL,
                operation_type TEXT NOT NULL,
                operation_state TEXT NOT NULL,
                effect_identity TEXT NOT NULL,
                external_id TEXT,
                scheduled_for TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (reminder_id) REFERENCES reminders(reminder_id),
                UNIQUE (reminder_id, effect_identity)
            )",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_operations_reminder_id ON reminder_operations(reminder_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_operations_operation_state ON reminder_operations(operation_state)",
            [],
        )?;

        // Suggestion eligibility table: eligibility scoring and rotation.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS suggestion_eligibility (
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
            "CREATE INDEX IF NOT EXISTS idx_suggestion_eligibility_eligible ON suggestion_eligibility(eligible)",
            [],
        )?;

        // Deletion work table: tracks cleanup work for deleted items.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS deletion_work (
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
            "CREATE INDEX IF NOT EXISTS idx_deletion_work_item_id ON deletion_work(item_id)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_deletion_work_status ON deletion_work(status)",
            [],
        )?;

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

    /// Migration step: v0 -> v1. Creates v1 schema tables and records version.
    /// This is used when upgrading from a database with version 0 or missing version record.
    fn migrate_to_v1(conn: &mut Connection, clock: &Arc<dyn Clock>) -> Result<()> {
        let tx = conn.transaction()?;

        // Create all v1 tables (using IF NOT EXISTS for idempotency).
        Self::create_tables_v1(&tx)?;

        // Record version 1 with upgrade timestamp.
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

    /// Get a reference to the connection (for queries in tests).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Get a mutable reference to the connection (for testing only).
    #[cfg(test)]
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}
