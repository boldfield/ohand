use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::sync::Arc;

use ohand_core::store::schema::{Clock, Database};

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

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

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create database and manually set a higher version.
    {
        let db = make_test_db(&path, instant)?;
        db.conn().execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![2, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
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

    // Manually insert a v2 record.
    {
        let conn = Connection::open(&path)?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![2, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
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
        assert_eq!(version, 1, "Schema version should be upgraded to 1");

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

#[test]
fn test_migration_failure_atomicity() -> Result<()> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_failure_atomicity_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create initial v1 database with a test source record.
    {
        let db = make_test_db(&path, instant)?;
        db.conn().execute(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params!["test-capture-1", "test text", "2026-01-15T10:30:00Z", "UTC", 0, "en", "gregorian", "personal", "route-1", 0, "2026-01-15T10:30:00Z"],
        )?;
    }

    // Verify the capture record survives reopening.
    {
        let db = make_test_db(&path, instant)?;
        let count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM captures WHERE capture_id = 'test-capture-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "Source capture should survive migration");
    }

    // Simulate interrupted migration by setting version to 0 and removing a table.
    {
        let conn = Connection::open(&path)?;
        conn.execute("DELETE FROM _schema_metadata", [])?;
        conn.execute(
            "INSERT INTO _schema_metadata (version, created_at, upgraded_at) VALUES (?, ?, ?)",
            rusqlite::params![0, "2026-01-15T10:00:00Z", "2026-01-15T10:00:00Z"],
        )?;
    }

    // Reopen should recover and recreate version 1 record atomically with tables.
    {
        let db = make_test_db(&path, instant)?;
        let version = db.schema_version()?;
        assert_eq!(version, 1, "Schema version should be restored");

        // Source record should still be intact even after migration.
        let count: i64 = db.conn().query_row(
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
fn test_migration_with_conflicting_schema_rolls_back() -> Result<()> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_rollback_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    // Create a v0 database with metadata and an old-schema captures table.
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
        // Create an old-shape captures table (missing required fields like route_id).
        conn.execute(
            "CREATE TABLE captures (
                capture_id TEXT PRIMARY KEY,
                text TEXT
            )",
            [],
        )?;
        conn.execute(
            "INSERT INTO captures (capture_id, text) VALUES (?, ?)",
            rusqlite::params!["test-capture-old", "old text"],
        )?;
    }

    // Try to migrate: should fail because of the conflicting schema.
    let result = make_test_db(&path, instant);
    assert!(
        result.is_err(),
        "Migration with conflicting schema should fail"
    );

    // Verify the database still contains the old captures record and no v1 tables were created.
    {
        let conn = Connection::open(&path)?;
        // Check that captures table still has only 2 columns (old schema).
        let mut stmt = conn.prepare("PRAGMA table_info(captures)")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            columns.len(),
            2,
            "Captures table should still have old schema (2 columns)"
        );

        // Check that old record survives.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM captures WHERE capture_id = 'test-capture-old'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            count, 1,
            "Old capture record should survive failed migration"
        );

        // Check that v1-only tables don't exist (no items table).
        let items_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='items')",
            [],
            |row| row.get(0),
        )?;
        assert!(
            !items_exists,
            "Items table should not exist after failed migration"
        );

        // Check that version is still 0 (not updated on failure).
        let version: u32 =
            conn.query_row("SELECT version FROM _schema_metadata", [], |row| row.get(0))?;
        assert_eq!(version, 0, "Version should remain 0 after failed migration");
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
