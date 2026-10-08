// Schema versioning and migrations
//
// This module handles SQLite schema creation, versioning, and atomic migrations.
// Schema state is produced only by an ordered list of migration steps. Each step runs in
// its own transaction together with the metadata row recording that step's own target
// version, so a failed step leaves the database at the previous version with no partial
// schema. A fresh database walks the same list starting from version 0.
//
// Version 0 (no `_schema_metadata` table, or a table with no rows above 0) means "no
// schema". Step 1 uses plain CREATE statements, so a version-0 database that already holds
// conflicting objects fails visibly and rolls back instead of being stamped as current.
// Follow-up schema changes append a new step to `MIGRATIONS`; they do not edit earlier steps.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction};
use std::collections::BTreeMap;
use std::sync::Arc;

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

/// Function applying one migration step's schema changes inside the step's transaction.
pub type MigrationFn = fn(&Transaction<'_>) -> Result<()>;

/// One ordered migration step. The version row for `target_version` is written by the
/// runner in the same transaction as `apply`.
#[derive(Clone, Copy)]
pub struct MigrationStep {
    pub target_version: u32,
    pub apply: MigrationFn,
}

/// Ordered migration steps. Target versions must start at 1 and increase by exactly 1.
pub const MIGRATIONS: &[MigrationStep] = &[
    MigrationStep {
        target_version: 1,
        apply: create_tables_v1,
    },
    MigrationStep {
        target_version: 2,
        apply: add_event_payload_columns_v2,
    },
];

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
    /// Open or create a database at the given path, applying pending `MIGRATIONS`.
    /// Databases with a newer schema version than supported are refused without being modified.
    pub fn open(path: &str, clock: Arc<dyn Clock>) -> Result<Self> {
        Self::open_with_migrations(path, clock, MIGRATIONS)
    }

    /// Open with an explicit ordered step list (used to exercise the migration envelope).
    pub fn open_with_migrations(
        path: &str,
        clock: Arc<dyn Clock>,
        steps: &[MigrationStep],
    ) -> Result<Self> {
        validate_step_list(steps)?;
        let latest_version = steps.last().map_or(0, |step| step.target_version);

        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // Set a reasonable busy timeout to allow concurrent transactions to wait
        // rather than immediately fail. 5 seconds should be sufficient for most operations.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        // Version check happens before any write to the database file.
        let current_version = Self::query_version(&conn)?;
        if current_version > latest_version {
            return Err(anyhow!(
                "Database schema version {} is newer than supported version {}. \
                 Please upgrade the application.",
                current_version,
                latest_version
            ));
        }

        if current_version > 0 {
            validate_schema_shape(&conn, steps, current_version)?;
        }

        for step in steps
            .iter()
            .filter(|step| step.target_version > current_version)
        {
            Self::apply_step(&mut conn, step, steps, &clock)?;
        }

        Ok(Database { conn })
    }

    /// Current schema version; 0 when the metadata table is absent or has no rows.
    fn query_version(conn: &Connection) -> Result<u32> {
        let metadata_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_schema_metadata')",
            [],
            |row| row.get(0),
        )?;
        if !metadata_exists {
            return Ok(0);
        }
        let version: Option<u32> =
            conn.query_row("SELECT MAX(version) FROM _schema_metadata", [], |row| {
                row.get(0)
            })?;
        Ok(version.unwrap_or(0))
    }

    /// Run one step and record its own target version in a single transaction.
    fn apply_step(
        conn: &mut Connection,
        step: &MigrationStep,
        steps: &[MigrationStep],
        clock: &Arc<dyn Clock>,
    ) -> Result<()> {
        let tx = conn.transaction()?;
        tx.execute(METADATA_TABLE_SQL, [])?;
        (step.apply)(&tx).map_err(|error| {
            anyhow!(
                "Migration to version {} failed: {}",
                step.target_version,
                error
            )
        })?;
        let now = clock.now().to_rfc3339();
        tx.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![step.target_version, now.clone(), now],
        )?;
        // The same check later opens apply, run before commit so a database that cannot pass
        // it (for example a pre-existing non-canonical ledger) is never stamped.
        validate_schema_shape(&tx, steps, step.target_version)?;
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

    /// Begin an IMMEDIATE transaction for idempotent operations.
    /// IMMEDIATE transactions upgrade to write lock immediately, preventing
    /// concurrent read-to-write escalation conflicts. This is needed for
    /// idempotent operations where check-then-insert must be atomic.
    pub fn immediate_transaction(&mut self) -> Result<Transaction<'_>> {
        self.conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| anyhow!(e))
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

fn validate_step_list(steps: &[MigrationStep]) -> Result<()> {
    for (index, step) in steps.iter().enumerate() {
        let expected = index as u32 + 1;
        if step.target_version != expected {
            return Err(anyhow!(
                "Migration step list is not ordered: step {} targets version {}, expected {}",
                index,
                step.target_version,
                expected
            ));
        }
    }
    Ok(())
}

/// Definition of the migration ledger table, created inside every step's transaction.
const METADATA_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS _schema_metadata (
    version INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    upgraded_at TEXT NOT NULL
)";

/// Object key (`type:name`) to its owning table and whitespace-normalized definition.
type SchemaSnapshot = BTreeMap<String, (String, String)>;

/// Every table, index, trigger and view definition stored in `sqlite_master`, which captures
/// column types, constraints, foreign keys, index uniqueness, indexed columns and partial
/// predicates exactly as written by the migration steps.
fn snapshot_schema(conn: &Connection) -> Result<SchemaSnapshot> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_master
         WHERE type IN ('table', 'index', 'trigger', 'view')
           AND name NOT LIKE 'sqlite_%' AND sql IS NOT NULL",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut snapshot = SchemaSnapshot::new();
    for (kind, name, table_name, sql) in rows {
        let normalized_sql = sql
            .replace('(', " ( ")
            .replace(')', " ) ")
            .replace(',', " , ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        snapshot.insert(format!("{}:{}", kind, name), (table_name, normalized_sql));
    }
    Ok(snapshot)
}

/// Verify that the database holds exactly the schema its version claims: every object
/// definition (including `_schema_metadata`) must match a reference built in memory from the
/// same steps up to `version`, and no extra table, index, trigger or view may exist. Extra
/// objects are rejected because a trigger or index on a source table can silently change what
/// is stored or enforced.
fn validate_schema_shape(conn: &Connection, steps: &[MigrationStep], version: u32) -> Result<()> {
    let mut reference = Connection::open_in_memory()?;
    for step in steps.iter().filter(|step| step.target_version <= version) {
        let tx = reference.transaction()?;
        tx.execute(METADATA_TABLE_SQL, [])?;
        (step.apply)(&tx)?;
        tx.commit()?;
    }
    let expected = snapshot_schema(&reference)?;
    let actual = snapshot_schema(conn)?;
    for (object, expected_definition) in &expected {
        match actual.get(object) {
            None => {
                return Err(anyhow!(
                    "Database claims schema version {} but is missing {}",
                    version,
                    object
                ))
            }
            Some(actual_definition) if actual_definition != expected_definition => {
                return Err(anyhow!(
                    "Database claims schema version {} but {} has an unexpected definition",
                    version,
                    object
                ))
            }
            Some(_) => {}
        }
    }
    if let Some(object) = actual.keys().find(|object| !expected.contains_key(*object)) {
        return Err(anyhow!(
            "Database claims schema version {} but contains unexpected {}",
            version,
            object
        ));
    }
    Ok(())
}

/// Step 1: initial M1 schema envelope.
fn create_tables_v1(tx: &Transaction<'_>) -> Result<()> {
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

    // Events table: lifecycle events (completion, cancellation, correction, suggestion-control).
    // Payload columns added in v2 migration.
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

    // Routes table: user-configured privacy and processing policy.
    tx.execute(
        "CREATE TABLE routes (
            route_id TEXT PRIMARY KEY,
            route_name TEXT NOT NULL,
            scope TEXT NOT NULL,
            processing_destinations TEXT NOT NULL,
            preview_safe INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        )",
        [],
    )?;
    tx.execute("CREATE INDEX idx_routes_scope ON routes(scope)", [])?;

    // Route authorizations table: per-capability authorization for routes.
    tx.execute(
        "CREATE TABLE route_authorizations (
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
        "CREATE INDEX idx_route_authorizations_route_id ON route_authorizations(route_id)",
        [],
    )?;

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
        "CREATE UNIQUE INDEX idx_proposals_applied_per_revision ON proposals(item_id, source_revision) WHERE applied_state = 'applied'",
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
    // Multiple versions can coexist per profile_id; jobs pin to a specific version.
    // profile_version is the immutable versioning key; profile_id groups related versions.
    // Note: No FK to jobs; profile can be deleted independently.
    // Jobs record the profile_version and fail visibly if it's no longer available.
    tx.execute(
        "CREATE TABLE provider_profiles (
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
        "CREATE INDEX idx_provider_profiles_profile_id ON provider_profiles(profile_id)",
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

    // Reminder operations table: durable record of scheduling/cancellation operations.
    tx.execute(
        "CREATE TABLE reminder_operations (
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
        "CREATE INDEX idx_reminder_operations_reminder_id ON reminder_operations(reminder_id)",
        [],
    )?;
    tx.execute(
        "CREATE INDEX idx_reminder_operations_operation_state ON reminder_operations(operation_state)",
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
            selection_sequence INTEGER,
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

    Ok(())
}

/// Step 2: Add event payload columns to events table for D03.
fn add_event_payload_columns_v2(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("ALTER TABLE events ADD COLUMN correction_kind TEXT", [])?;
    tx.execute(
        "ALTER TABLE events ADD COLUMN correction_old_value TEXT",
        [],
    )?;
    tx.execute(
        "ALTER TABLE events ADD COLUMN correction_new_value TEXT",
        [],
    )?;
    tx.execute(
        "ALTER TABLE events ADD COLUMN suggestion_control_kind TEXT",
        [],
    )?;
    Ok(())
}
