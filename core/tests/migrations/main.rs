use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction};
use std::sync::Arc;

use ohand_core::store::schema::{Clock, Database, MigrationFn, MigrationStep, MIGRATIONS};

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let db = make_test_db(&path, instant)?;
    assert_eq!(db.schema_version()?, 4);

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let _db = make_test_db(&path, instant)?;
    }

    {
        let db = make_test_db(&path, instant)?;
        assert_eq!(db.schema_version()?, 4);
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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create database and manually set a higher version.
    {
        let db = make_test_db(&path, instant)?;
        db.conn().execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![5, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create a v1 database.
    {
        let _db = make_test_db(&path, instant)?;
    }

    // Manually insert a v5 record (higher than supported).
    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![5, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
        )?;
    }
    let bytes_before = std::fs::read(&path)?;

    // Try to open: should fail without modifying the file.
    let _ = make_test_db(&path, instant);
    let bytes_after = std::fs::read(&path)?;

    // File bytes should not have changed (no WAL or new tables created).
    assert_eq!(
        bytes_before, bytes_after,
        "Database file was modified during version check"
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_migration_from_version_zero() -> Result<()> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_migration_v0_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create a minimal v0 database with metadata but no tables.
    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![0, "2026-01-15T10:00:00Z", "2026-01-15T10:00:00Z"],
        )?;
    }

    // Open with migration: should create v1 tables and update version.
    {
        let db = make_test_db(&path, instant)?;
        let version = db.schema_version()?;
        assert_eq!(version, 4, "Schema version should be upgraded to 4");

        // Check that v1 tables now exist.
        let tables_exist: bool = db.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='captures')",
            [],
            |row| row.get(0),
        )?;
        assert!(tables_exist, "captures table should exist after migration");
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn insert_source_capture(conn: &Connection, capture_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![capture_id, "test text", "2026-01-15T10:30:00Z", "UTC", 0, "en", "gregorian", "personal", "route-1", 0, "2026-01-15T10:30:00Z"],
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get(0),
    )?)
}

fn capture_count(conn: &Connection, capture_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM captures WHERE capture_id = ?",
        [capture_id],
        |row| row.get(0),
    )?)
}

fn create_synthetic_v2_table(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("CREATE TABLE synthetic_v2 (id TEXT PRIMARY KEY)", [])?;
    Ok(())
}

fn fail_after_ddl(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("CREATE TABLE synthetic_partial (id TEXT PRIMARY KEY)", [])?;
    tx.execute(
        "ALTER TABLE captures ADD COLUMN synthetic_partial_col TEXT",
        [],
    )?;
    tx.execute("INSERT INTO synthetic_missing_table VALUES (1)", [])?;
    Ok(())
}

fn steps_with(extra: MigrationFn) -> Vec<MigrationStep> {
    let mut steps = MIGRATIONS.to_vec();
    steps.push(MigrationStep {
        target_version: steps.len() as u32 + 1,
        apply: extra,
    });
    steps
}

fn open_with(path: &str, instant: DateTime<Utc>, steps: &[MigrationStep]) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(TestClock { instant });
    Database::open_with_migrations(path, clock, steps)
}

#[test]
fn test_synthetic_step_success_records_own_version() -> Result<()> {
    let path = temp_db_path("step_success");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let later = DateTime::parse_from_rfc3339("2026-02-01T08:00:00+00:00")?.with_timezone(&Utc);

    {
        let db = make_test_db(&path, instant)?;
        insert_source_capture(db.conn(), "capture-keep")?;
    }

    let steps = steps_with(create_synthetic_v2_table);
    {
        let db = open_with(&path, later, &steps)?;
        assert_eq!(db.schema_version()?, 5);
        assert!(table_exists(db.conn(), "synthetic_v2")?);
        assert_eq!(capture_count(db.conn(), "capture-keep")?, 1);
        let (created, version_two): (String, String) = db.conn().query_row(
            "SELECT (SELECT created_at FROM _schema_metadata WHERE version = 5),
                    (SELECT created_at FROM _schema_metadata WHERE version = 2)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(created.starts_with("2026-02-01"), "Got: {}", created);
        assert!(
            version_two.starts_with("2026-01-15"),
            "Got: {}",
            version_two
        );
    }

    // The default (v1-only) list now refuses the v2 database.
    let refused = make_test_db(&path, instant);
    assert!(refused
        .unwrap_err()
        .to_string()
        .contains("newer than supported"));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_fresh_database_walks_full_step_list() -> Result<()> {
    let path = temp_db_path("fresh_walk");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let steps = steps_with(create_synthetic_v2_table);
    let db = open_with(&path, instant, &steps)?;
    assert_eq!(db.schema_version()?, 5);
    assert!(table_exists(db.conn(), "captures")?);
    assert!(table_exists(db.conn(), "synthetic_v2")?);
    let versions: i64 =
        db.conn()
            .query_row("SELECT COUNT(*) FROM _schema_metadata", [], |row| {
                row.get(0)
            })?;
    assert_eq!(versions, 5);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_failed_step_rolls_back_and_preserves_source_records() -> Result<()> {
    let path = temp_db_path("step_failure");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let db = make_test_db(&path, instant)?;
        insert_source_capture(db.conn(), "capture-keep")?;
    }

    let steps = steps_with(fail_after_ddl);
    let result = open_with(&path, instant, &steps);
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("Migration to version 5 failed"),
        "Got: {}",
        error
    );

    {
        let conn = Connection::open(&path)?;
        let version: u32 =
            conn.query_row("SELECT MAX(version) FROM _schema_metadata", [], |row| {
                row.get(0)
            })?;
        assert_eq!(version, 4, "Version must stay at the last good version");
        assert!(!table_exists(&conn, "synthetic_partial")?);
        let column_added: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('captures') WHERE name='synthetic_partial_col')",
            [],
            |row| row.get(0),
        )?;
        assert!(!column_added, "Partial ALTER must be rolled back");
        assert_eq!(capture_count(&conn, "capture-keep")?, 1);
    }

    // The original database still opens normally.
    let db = make_test_db(&path, instant)?;
    assert_eq!(db.schema_version()?, 4);
    assert_eq!(capture_count(db.conn(), "capture-keep")?, 1);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_failed_initial_migration_leaves_no_partial_schema() -> Result<()> {
    let path = temp_db_path("initial_failure");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    fn fail_initial(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("CREATE TABLE partial_one (id TEXT)", [])?;
        tx.execute("CREATE TABLE partial_one (id TEXT)", [])?;
        Ok(())
    }
    let steps = [MigrationStep {
        target_version: 1,
        apply: fail_initial,
    }];
    assert!(open_with(&path, instant, &steps).is_err());

    let conn = Connection::open(&path)?;
    assert!(!table_exists(&conn, "partial_one")?);
    assert!(!table_exists(&conn, "_schema_metadata")?);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_unordered_step_list_rejected() -> Result<()> {
    let path = temp_db_path("unordered");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let steps = [MigrationStep {
        target_version: 2,
        apply: create_synthetic_v2_table,
    }];
    assert!(open_with(&path, instant, &steps).is_err());
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_version_row_without_schema_rejected() -> Result<()> {
    let path = temp_db_path("stamp_only");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (1, 'x', 'x')",
            [],
        )?;
    }

    let error = make_test_db(&path, instant).unwrap_err().to_string();
    assert!(error.contains("missing"), "Got: {}", error);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_malformed_table_at_claimed_version_rejected() -> Result<()> {
    let path = temp_db_path("malformed");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let db = make_test_db(&path, instant)?;
        db.conn()
            .execute("ALTER TABLE deletion_work DROP COLUMN work_type", [])?;
    }

    let error = make_test_db(&path, instant).unwrap_err().to_string();
    assert!(error.contains("unexpected definition"), "Got: {}", error);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_same_name_weaker_index_at_claimed_version_rejected() -> Result<()> {
    let path = temp_db_path("weak_index");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let db = make_test_db(&path, instant)?;
        db.conn()
            .execute("DROP INDEX idx_proposals_applied_per_revision", [])?;
        db.conn().execute(
            "CREATE INDEX idx_proposals_applied_per_revision ON proposals(item_id)",
            [],
        )?;
    }

    let error = make_test_db(&path, instant).unwrap_err().to_string();
    assert!(
        error.contains("index:idx_proposals_applied_per_revision")
            && error.contains("unexpected definition"),
        "Got: {}",
        error
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_malformed_metadata_table_at_claimed_version_rejected() -> Result<()> {
    let path = temp_db_path("bad_metadata");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let db = make_test_db(&path, instant)?;
        db.conn().execute("DROP TABLE _schema_metadata", [])?;
        db.conn().execute(
            "CREATE TABLE _schema_metadata (version INTEGER PRIMARY KEY)",
            [],
        )?;
        db.conn()
            .execute("INSERT INTO _schema_metadata (version) VALUES (1)", [])?;
    }

    let error = make_test_db(&path, instant).unwrap_err().to_string();
    assert!(
        error.contains("table:_schema_metadata") && error.contains("unexpected definition"),
        "Got: {}",
        error
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_version_zero_with_conflicting_late_table_rolls_back() -> Result<()> {
    let path = temp_db_path("late_conflict");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Version 0 database holding a malformed table that conflicts only at the end of step 1.
    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE _schema_metadata (
                version INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL,
                upgraded_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (0, 'x', 'x')",
            [],
        )?;
        conn.execute(
            "CREATE TABLE deletion_work (deletion_work_id TEXT PRIMARY KEY, note TEXT)",
            [],
        )?;
        conn.execute("INSERT INTO deletion_work VALUES ('old', 'keep me')", [])?;
    }

    assert!(make_test_db(&path, instant).is_err());

    let conn = Connection::open(&path)?;
    for table in [
        "captures",
        "items",
        "jobs",
        "proposals",
        "provider_profiles",
    ] {
        assert!(
            !table_exists(&conn, table)?,
            "{} must be rolled back after late failure",
            table
        );
    }
    let kept: i64 = conn.query_row("SELECT COUNT(*) FROM deletion_work", [], |row| row.get(0))?;
    assert_eq!(kept, 1);
    let version: u32 = conn.query_row("SELECT MAX(version) FROM _schema_metadata", [], |row| {
        row.get(0)
    })?;
    assert_eq!(version, 0);

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
    let created_at: String = db.conn().query_row(
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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    let expected_tables = vec![
        "captures",
        "items",
        "corrections",
        "events",
        "routes",
        "route_authorizations",
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
        let exists: bool = db.conn().query_row(
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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    // Check that proposals table has the required columns.
    let mut stmt = db.conn().prepare("PRAGMA table_info(proposals)")?;

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    // Check that jobs table has the required columns.
    let mut stmt = db.conn().prepare("PRAGMA table_info(jobs)")?;

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    // Check that items table has the required columns.
    let mut stmt = db.conn().prepare("PRAGMA table_info(items)")?;

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

#[test]
fn test_routes_and_authorizations_exist() -> Result<()> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_routes_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    // Check that routes table exists with required columns.
    let mut stmt = db.conn().prepare("PRAGMA table_info(routes)")?;
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get(1))?
        .collect::<Result<Vec<_>, _>>()?;

    assert!(
        columns.contains(&"route_id".to_string()),
        "route_id should exist"
    );
    assert!(columns.contains(&"scope".to_string()), "scope should exist");

    // Check that route_authorizations table exists.
    let mut stmt = db
        .conn()
        .prepare("PRAGMA table_info(route_authorizations)")?;
    let auth_columns: Vec<String> = stmt
        .query_map([], |row| row.get(1))?
        .collect::<Result<Vec<_>, _>>()?;

    assert!(
        auth_columns.contains(&"route_id".to_string()),
        "route_id should exist in authorizations"
    );
    assert!(
        auth_columns.contains(&"capability".to_string()),
        "capability should exist in authorizations"
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_provider_profiles_versioning() -> Result<()> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_profile_versions_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let db = make_test_db(&path, instant)?;

    // Insert two versions of the same profile (same profile_id, different profile_version).
    db.conn().execute(
        "INSERT INTO provider_profiles (
            profile_version, profile_id, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prof-123-v1",
            "prof-123",
            "test",
            "model-1",
            30,
            "exponential",
            "[]",
            "[]",
            "2026-01-15T10:30:00Z"
        ],
    )?;

    db.conn().execute(
        "INSERT INTO provider_profiles (
            profile_version, profile_id, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prof-123-v2",
            "prof-123",
            "test",
            "model-2",
            60,
            "exponential",
            "[]",
            "[]",
            "2026-01-15T10:31:00Z"
        ],
    )?;

    // Verify both versions exist with the same profile_id.
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM provider_profiles WHERE profile_id = 'prof-123'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count, 2,
        "Should be able to store multiple versions per profile_id"
    );

    // Verify they have different profile_version values.
    let versions: Vec<String> = db
        .conn()
        .prepare("SELECT profile_version FROM provider_profiles WHERE profile_id = 'prof-123' ORDER BY profile_version")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(versions, vec!["prof-123-v1", "prof-123-v2"]);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn instant_for_tests() -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc))
}

fn assert_first_open_of_version_zero_fails_unchanged(ledger_sql: &str, label: &str) -> Result<()> {
    let path = temp_db_path(label);
    {
        let conn = Connection::open(&path)?;
        conn.execute(ledger_sql, [])?;
        conn.execute("CREATE TABLE legacy_notes (note TEXT)", [])?;
        conn.execute("INSERT INTO legacy_notes VALUES ('keep me')", [])?;
    }

    let result = make_test_db(&path, instant_for_tests()?);
    assert!(
        result.is_err(),
        "non-canonical ledger must fail on the first open"
    );

    let conn = Connection::open(&path)?;
    assert!(!table_exists(&conn, "captures")?);
    let ledger_rows: i64 = conn.query_row("SELECT COUNT(*) FROM _schema_metadata", [], |row| {
        row.get(0)
    })?;
    assert_eq!(ledger_rows, 0, "no version row may be recorded");
    let kept: i64 = conn.query_row("SELECT COUNT(*) FROM legacy_notes", [], |row| row.get(0))?;
    assert_eq!(kept, 1);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_version_zero_with_extra_column_ledger_fails_first_open() -> Result<()> {
    assert_first_open_of_version_zero_fails_unchanged(
        "CREATE TABLE _schema_metadata (version INTEGER PRIMARY KEY, created_at TEXT NOT NULL, upgraded_at TEXT NOT NULL, note TEXT)",
        "ledger_extra_column",
    )
}

#[test]
fn test_version_zero_with_wrong_constraint_ledger_fails_first_open() -> Result<()> {
    assert_first_open_of_version_zero_fails_unchanged(
        "CREATE TABLE _schema_metadata (version INTEGER PRIMARY KEY, created_at TEXT, upgraded_at TEXT)",
        "ledger_wrong_constraint",
    )
}

#[test]
fn test_ledger_whitespace_differences_do_not_brick_database() -> Result<()> {
    let path = temp_db_path("ledger_whitespace");
    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE _schema_metadata(version INTEGER PRIMARY KEY,created_at TEXT NOT NULL,upgraded_at TEXT NOT NULL)",
            [],
        )?;
    }
    let instant = instant_for_tests()?;
    drop(make_test_db(&path, instant)?);
    let reopened = make_test_db(&path, instant)?;
    assert_eq!(reopened.schema_version()?, 4);
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn assert_unexpected_object_rejected(extra_sql: &str, label: &str) -> Result<()> {
    let path = temp_db_path(label);
    let instant = instant_for_tests()?;
    {
        let db = make_test_db(&path, instant)?;
        insert_source_capture(db.conn(), "capture-1")?;
        db.conn().execute(extra_sql, [])?;
    }

    let error = make_test_db(&path, instant).expect_err("unexpected schema object must be refused");
    assert!(
        error.to_string().contains("unexpected"),
        "unexpected error: {}",
        error
    );

    let conn = Connection::open(&path)?;
    assert_eq!(capture_count(&conn, "capture-1")?, 1);
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_unexpected_trigger_on_source_table_rejected() -> Result<()> {
    assert_unexpected_object_rejected(
        "CREATE TRIGGER drop_captures AFTER INSERT ON captures BEGIN DELETE FROM captures; END",
        "extra_trigger",
    )
}

#[test]
fn test_unexpected_view_rejected() -> Result<()> {
    assert_unexpected_object_rejected(
        "CREATE VIEW extra_view AS SELECT capture_id FROM captures",
        "extra_view",
    )
}

#[test]
fn test_unexpected_index_and_table_rejected() -> Result<()> {
    assert_unexpected_object_rejected(
        "CREATE INDEX idx_extra_captures ON captures(locale)",
        "extra_index",
    )?;
    assert_unexpected_object_rejected("CREATE TABLE stray_table (id TEXT)", "extra_table")
}

#[test]
fn test_v1_to_v2_migration_preserves_data() -> Result<()> {
    let path = temp_db_path("v1_to_v2_upgrade");
    let instant = instant_for_tests()?;

    // Create a v1 database with source data
    {
        let db = open_with(&path, instant, &MIGRATIONS[..1])?;
        assert_eq!(db.schema_version()?, 1);

        insert_source_capture(db.conn(), "cap-1")?;
        db.conn().execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
            rusqlite::params!["item-1", "cap-1", "2026-01-15T10:30:00Z", "2026-01-15T10:30:00Z"],
        )?;
        db.conn().execute(
            "INSERT INTO events (event_id, item_id, revision, event_type, happened_at)
             VALUES (?, ?, ?, ?, ?)",
            rusqlite::params!["evt-1", "item-1", 0, "completion", "2026-01-15T10:30:00Z"],
        )?;
    }

    // Reopen the database with full migration list: should upgrade from v1 to v2
    {
        let db = make_test_db(&path, instant)?;
        assert_eq!(db.schema_version()?, 4);

        // Verify source data is intact
        let capture_count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM captures WHERE capture_id = 'cap-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            capture_count, 1,
            "Capture should be preserved after upgrade"
        );

        let item_count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM items WHERE item_id = 'item-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(item_count, 1, "Item should be preserved after upgrade");

        let event_count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM events WHERE event_id = 'evt-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(event_count, 1, "Event should be preserved after upgrade");

        // Verify v2 columns now exist (but are NULL for existing events)
        let correction_kind: Option<String> = db.conn().query_row(
            "SELECT correction_kind FROM events WHERE event_id = 'evt-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            correction_kind, None,
            "v2 payload columns should exist and be NULL for existing events"
        );
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_v2_to_v3_migration_preserves_reminder_rows() -> Result<()> {
    let path = temp_db_path("v2_to_v3_upgrade");
    let instant = instant_for_tests()?;

    {
        let db = open_with(&path, instant, &MIGRATIONS[..2])?;
        assert_eq!(db.schema_version()?, 2);

        insert_source_capture(db.conn(), "cap-1")?;
        db.conn().execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'processed', 'not_applicable', ?, ?)",
            rusqlite::params!["item-1", "cap-1", "2026-01-15T10:30:00Z", "2026-01-15T10:30:00Z"],
        )?;
        db.conn().execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, acknowledgment_state, created_at, updated_at)
             VALUES ('rem-1', 'item-1', 'resolved', 'scheduled', 'unknown', 'not_acknowledged', ?, ?)",
            rusqlite::params!["2026-01-15T10:30:00Z", "2026-01-15T10:30:00Z"],
        )?;
    }

    {
        let db = make_test_db(&path, instant)?;
        assert_eq!(db.schema_version()?, 4);
        let (request_state, schedule_state, unschedulable_reason): (String, String, Option<String>) =
            db.conn().query_row(
                "SELECT request_state, schedule_state, unschedulable_reason FROM reminders WHERE reminder_id = 'rem-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(request_state, "resolved");
        assert_eq!(schedule_state, "scheduled");
        assert_eq!(
            unschedulable_reason, None,
            "existing reminder rows gain a NULL unschedulable_reason"
        );
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_v3_to_v4_migration_preserves_provider_profiles() -> Result<()> {
    let path = temp_db_path("v3_to_v4_upgrade");
    let instant = instant_for_tests()?;

    {
        let db = open_with(&path, instant, &MIGRATIONS[..3])?;
        assert_eq!(db.schema_version()?, 3);
        db.conn().execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at)
             VALUES ('prof-1-v1', 'prof-1', 'anthropic', 'https://provider.example', 'synthetic-model', 'keychain:synthetic', 30, 'default', '[]', '[]', ?)",
            rusqlite::params!["2026-01-15T10:30:00Z"],
        )?;
    }

    {
        let db = make_test_db(&path, instant)?;
        assert_eq!(db.schema_version()?, 4);
        let (credential_ref, revoked_at): (Option<String>, Option<String>) = db.conn().query_row(
            "SELECT credential_ref, revoked_at FROM provider_profiles WHERE profile_version = 'prof-1-v1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(credential_ref.as_deref(), Some("keychain:synthetic"));
        assert_eq!(
            revoked_at, None,
            "existing profiles stay unrevoked after the upgrade"
        );
        assert!(table_exists(db.conn(), "job_requeues")?);
        let requeue_rows: i64 =
            db.conn()
                .query_row("SELECT COUNT(*) FROM job_requeues", [], |row| row.get(0))?;
        assert_eq!(requeue_rows, 0);
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}
