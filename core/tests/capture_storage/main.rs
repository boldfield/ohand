use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use ohand_core::store::captures::{get_capture, save_capture, Capture};
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

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_captures_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn make_test_capture(id: &str) -> Capture {
    Capture::new(
        id.to_string(),
        Some("test text".to_string()),
        None,
        "2026-01-15T10:30:00Z".to_string(),
        "America/New_York".to_string(),
        -300,
        "en".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-1".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        None,
    )
    .unwrap()
}

fn make_test_capture_with_audio(id: &str) -> Capture {
    Capture::new(
        id.to_string(),
        None,
        Some("audio:reference:123".to_string()),
        "2026-01-15T10:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-1".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        None,
    )
    .unwrap()
}

#[test]
fn test_save_and_retrieve_capture() -> Result<()> {
    let path = temp_db_path("save_retrieve");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = make_test_capture("cap-1");
    let tx = db.transaction()?;
    let saved = save_capture(&tx, &capture)?;
    tx.commit()?;

    assert_eq!(saved, capture);

    let tx = db.transaction()?;
    let retrieved = get_capture(&tx, "cap-1")?;
    assert_eq!(retrieved, Some(capture));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_idempotent_save_returns_same_record() -> Result<()> {
    let path = temp_db_path("idempotent");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = make_test_capture("cap-1");

    // First save
    let tx = db.transaction()?;
    let saved1 = save_capture(&tx, &capture)?;
    tx.commit()?;

    // Second save with identical capture
    let tx = db.transaction()?;
    let saved2 = save_capture(&tx, &capture)?;
    tx.commit()?;

    assert_eq!(saved1, saved2);
    assert_eq!(saved1, capture);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_conflict_on_different_content_same_id() -> Result<()> {
    let path = temp_db_path("conflict");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture1 = make_test_capture("cap-1");
    let tx = db.transaction()?;
    save_capture(&tx, &capture1)?;
    tx.commit()?;

    // Try to save different capture with same ID
    let mut capture2 = make_test_capture("cap-1");
    capture2.text = Some("different text".to_string());

    let tx = db.transaction()?;
    let result = save_capture(&tx, &capture2);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("already exists with different content"));
    tx.rollback()?;

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_capture_with_text_and_audio() -> Result<()> {
    let path = temp_db_path("text_and_audio");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = Capture::new(
        "cap-1".to_string(),
        Some("text content".to_string()),
        Some("audio:ref".to_string()),
        "2026-01-15T10:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-1".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        Some("therapy".to_string()),
    )?;

    let tx = db.transaction()?;
    let saved = save_capture(&tx, &capture)?;
    tx.commit()?;

    assert_eq!(saved.text, Some("text content".to_string()));
    assert_eq!(saved.audio_reference, Some("audio:ref".to_string()));
    assert_eq!(saved.session_topic, Some("therapy".to_string()));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_metadata_persists_across_reopen() -> Result<()> {
    let path = temp_db_path("persist_reopen");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    {
        let mut db = make_test_db(&path, instant)?;
        let capture = make_test_capture("cap-1");
        let tx = db.transaction()?;
        save_capture(&tx, &capture)?;
        tx.commit()?;
    }

    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        let retrieved = get_capture(&tx, "cap-1")?;
        assert!(retrieved.is_some());
        let cap = retrieved.unwrap();
        assert_eq!(cap.timezone_id, "America/New_York");
        assert_eq!(cap.utc_offset_minutes, -300);
        assert_eq!(cap.locale, "en");
        assert_eq!(cap.item_scope, "personal");
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_privacy_metadata_persists() -> Result<()> {
    let path = temp_db_path("privacy_metadata");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let capture = Capture::new(
        "cap-1".to_string(),
        Some("private content".to_string()),
        None,
        "2026-01-15T10:30:00Z".to_string(),
        "Europe/London".to_string(),
        0,
        "en-GB".to_string(),
        "gregorian".to_string(),
        "session".to_string(),
        "route-private".to_string(),
        true,
        "2026-01-15T10:30:00Z".to_string(),
        Some("therapy".to_string()),
    )?;

    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        save_capture(&tx, &capture)?;
        tx.commit()?;
    }

    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        let retrieved = get_capture(&tx, "cap-1")?;
        let cap = retrieved.unwrap();
        assert_eq!(cap.item_scope, "session");
        assert_eq!(cap.route_id, "route-private");
        assert!(cap.entry_locked);
        assert_eq!(cap.session_topic, Some("therapy".to_string()));
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_audio_only_capture() -> Result<()> {
    let path = temp_db_path("audio_only");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = make_test_capture_with_audio("cap-audio");
    let tx = db.transaction()?;
    let saved = save_capture(&tx, &capture)?;
    tx.commit()?;

    assert_eq!(saved.text, None);
    assert_eq!(
        saved.audio_reference,
        Some("audio:reference:123".to_string())
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_multiple_different_captures() -> Result<()> {
    let path = temp_db_path("multiple");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let cap1 = make_test_capture("cap-1");
    let mut cap2 = make_test_capture("cap-2");
    cap2.text = Some("different text".to_string());

    {
        let tx = db.transaction()?;
        save_capture(&tx, &cap1)?;
        save_capture(&tx, &cap2)?;
        tx.commit()?;
    }

    {
        let tx = db.transaction()?;
        let retrieved1 = get_capture(&tx, "cap-1")?;
        let retrieved2 = get_capture(&tx, "cap-2")?;

        assert_eq!(retrieved1.unwrap().text, Some("test text".to_string()));
        assert_eq!(retrieved2.unwrap().text, Some("different text".to_string()));
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_durability_abort_before_commit() -> Result<()> {
    let path = temp_db_path("durability_abort");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = make_test_capture("cap-1");
    {
        let tx = db.transaction()?;
        save_capture(&tx, &capture)?;
        tx.rollback()?;
    }

    // Verify capture was NOT persisted
    let tx = db.transaction()?;
    let retrieved = get_capture(&tx, "cap-1")?;
    assert_eq!(retrieved, None);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_durability_persists_after_commit() -> Result<()> {
    let path = temp_db_path("durability_commit");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let capture = make_test_capture("cap-1");

    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        save_capture(&tx, &capture)?;
        tx.commit()?;
    }

    // Reopen and verify capture persists
    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        let retrieved = get_capture(&tx, "cap-1")?;
        assert_eq!(retrieved, Some(capture.clone()));
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_ack_only_after_transaction_commit() -> Result<()> {
    let path = temp_db_path("ack_after_commit");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = make_test_capture("cap-ack");

    {
        let tx = db.transaction()?;
        let _saved = save_capture(&tx, &capture)?;
        tx.rollback()?;
    }

    // After rollback, verify it was not persisted
    {
        let tx = db.transaction()?;
        let retrieved = get_capture(&tx, "cap-ack")?;
        assert_eq!(retrieved, None);
    }

    // Now commit properly
    {
        let tx = db.transaction()?;
        save_capture(&tx, &capture)?;
        tx.commit()?;
    }

    // Verify it persists
    {
        let tx = db.transaction()?;
        let retrieved = get_capture(&tx, "cap-ack")?;
        assert_eq!(retrieved, Some(capture));
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_constraint_requires_text_or_audio() -> Result<()> {
    let result = Capture::new(
        "cap-empty".to_string(),
        None,
        None,
        "2026-01-15T10:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-1".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        None,
    );

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("text or audio_reference"));

    Ok(())
}

#[test]
fn test_idempotent_conflict_different_scope() -> Result<()> {
    let path = temp_db_path("conflict_scope");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture1 = make_test_capture("cap-1");
    let tx = db.transaction()?;
    save_capture(&tx, &capture1)?;
    tx.commit()?;

    // Try to save with different scope
    let mut capture2 = make_test_capture("cap-1");
    capture2.item_scope = "work".to_string();

    let tx = db.transaction()?;
    let result = save_capture(&tx, &capture2);
    assert!(result.is_err());
    tx.rollback()?;

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_retrieved_capture_matches_saved() -> Result<()> {
    let path = temp_db_path("match");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let capture = Capture::new(
        "cap-match".to_string(),
        Some("original text".to_string()),
        Some("audio:file:456".to_string()),
        "2026-01-15T14:30:00Z".to_string(),
        "America/Los_Angeles".to_string(),
        -480,
        "en-US".to_string(),
        "gregorian".to_string(),
        "work".to_string(),
        "route-work".to_string(),
        true,
        "2026-01-15T14:30:00Z".to_string(),
        Some("project-alpha".to_string()),
    )?;

    let tx = db.transaction()?;
    let saved = save_capture(&tx, &capture)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let retrieved = get_capture(&tx, "cap-match")?;

    assert_eq!(retrieved, Some(capture.clone()));
    assert_eq!(saved, capture);

    let _ = std::fs::remove_file(&path);
    Ok(())
}
