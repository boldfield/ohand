// Integration tests for full-text search indexing.
//
// These tests verify:
// 1. Transactional index maintenance during capture/correction/deletion
// 2. Crash/rebuild resilience: index can be rebuilt without inventing/resurrecting records
// 3. Corrected text takes precedence, preventing model summaries from being presented as original
// 4. Index rebuild and direct-source fallback agree on accessible records

use anyhow::Result;
use std::sync::Arc;

// Mock clock for testing
struct TestClock;
impl ohand_core::store::schema::Clock for TestClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(0).unwrap()
    }
}

#[test]
fn test_transactional_index_on_capture() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'hello world', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index the capture.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Verify the index entry exists.
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);

    // Verify text is indexed correctly.
    let text: String = tx.query_row(
        "SELECT original_text FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(text, "hello world");

    tx.commit()?;
    Ok(())
}

#[test]
fn test_corrected_text_takes_precedence() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture with original text.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'original text', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index original.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Add a user correction (not a model summary).
    tx.execute(
        "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
         VALUES ('corr1', 'item1', 0, 'text', 'original text', 'corrected by user', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index the correction.
    ohand_core::retrieval::index::index_text_correction(&tx, "item1")?;

    // Create a search result to demonstrate display_text prioritizes correction.
    let search_result = ohand_core::retrieval::index::SearchResult {
        item_id: "item1".to_string(),
        capture_id: "cap1".to_string(),
        item_scope: "personal".to_string(),
        original_text: Some("original text".to_string()),
        corrected_text: Some("corrected by user".to_string()),
        source_type: "corrected".to_string(),
    };

    // Corrected text takes precedence (not original), preventing model summaries
    // from being presented as original quotations.
    assert_eq!(search_result.display_text(), Some("corrected by user"));

    tx.commit()?;
    Ok(())
}

#[test]
fn test_deleted_items_not_indexed() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'secret data', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Rebuild index (should include active items).
    ohand_core::retrieval::index::rebuild_index(&tx)?;

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "Active item should be indexed");

    // Mark item as deleted.
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = 'item1'",
        [],
    )?;

    // Rebuild index (should exclude deleted items).
    ohand_core::retrieval::index::rebuild_index(&tx)?;

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0, "Deleted item should not be indexed");

    tx.commit()?;
    Ok(())
}

#[test]
fn test_crash_rebuild_resilience() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create three captures: one active, one completed, one deleted.
    for (i, (lifecycle_state, _should_be_indexed)) in
        [("active", true), ("completed", true), ("deleted", false)]
            .iter()
            .enumerate()
    {
        let item_id = format!("item{}", i + 1);
        let cap_id = format!("cap{}", i + 1);

        tx.execute(
            "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
             timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
             VALUES (?, ?, 'personal', 'route1', '2026-01-01T00:00:00Z',
             'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
            rusqlite::params![&cap_id, format!("text {}", i)],
        )?;
        tx.execute(
            "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
             processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, ?, 'saved', 'not_configured',
             'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![&item_id, &cap_id, lifecycle_state],
        )?;
    }

    // Rebuild index from scratch (simulating restart after crash).
    ohand_core::retrieval::index::rebuild_index(&tx)?;

    // Verify correct items are indexed (not deleted, not invented/resurrected).
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "Active item should be indexed");

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "Completed item should be indexed");

    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item3'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0, "Deleted item should NOT be indexed");

    // Verify no extra index entries (cannot invent records).
    let total: i64 = tx.query_row("SELECT COUNT(*) FROM search_index", [], |row| row.get(0))?;
    assert_eq!(total, 2, "Only 2 items should be indexed");

    tx.commit()?;
    Ok(())
}

#[test]
fn test_remove_deleted_item_from_index() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create and index an item.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'test', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Verify it's indexed.
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);

    // Remove from index.
    ohand_core::retrieval::index::remove_item_from_index(&tx, "item1")?;

    // Verify it's gone.
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);

    tx.commit()?;
    Ok(())
}

#[test]
fn test_index_integrity_detects_missing_entries() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture and item.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'text', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Don't index it - leave the index empty.

    // Verify integrity check detects the missing entry.
    let integrity = ohand_core::retrieval::index::verify_index_integrity(&tx)?;
    match integrity {
        ohand_core::retrieval::index::IndexIntegrityIssue::MissingEntries(count) => {
            assert_eq!(count, 1, "Should detect 1 missing index entry");
        }
        _ => panic!("Expected MissingEntries issue"),
    }

    tx.commit()?;
    Ok(())
}

#[test]
fn test_index_integrity_healthy() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create and properly index a capture.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'text', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Verify integrity check passes.
    let integrity = ohand_core::retrieval::index::verify_index_integrity(&tx)?;
    assert_eq!(
        integrity,
        ohand_core::retrieval::index::IndexIntegrityIssue::Healthy
    );

    tx.commit()?;
    Ok(())
}

#[test]
fn test_multiple_corrections_converge_with_rebuild() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture with original text.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'buy milk', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index original.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // First correction.
    tx.execute(
        "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
         VALUES ('corr1', 'item1', 0, 'text', 'buy milk', 'buy eggs', '2026-01-01T00:00:00Z')",
        [],
    )?;
    ohand_core::retrieval::index::index_text_correction(&tx, "item1")?;

    // Second correction.
    tx.execute(
        "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
         VALUES ('corr2', 'item1', 1, 'text', 'buy eggs', 'buy bread', '2026-01-01T01:00:00Z')",
        [],
    )?;
    ohand_core::retrieval::index::index_text_correction(&tx, "item1")?;

    // Verify incremental index after multiple corrections.
    let count_eggs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1' AND current_text MATCH 'eggs'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count_eggs, 0,
        "Superseded correction should not be searchable"
    );

    let count_bread: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1' AND current_text MATCH 'bread'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count_bread, 1, "Latest correction should be searchable");

    // Rebuild index and verify it agrees.
    ohand_core::retrieval::index::rebuild_index(&tx)?;

    let count_eggs_rebuilt: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1' AND current_text MATCH 'eggs'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count_eggs_rebuilt, 0,
        "Rebuilt index should not have superseded correction"
    );

    let count_bread_rebuilt: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1' AND current_text MATCH 'bread'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count_bread_rebuilt, 1,
        "Rebuilt index should have latest correction"
    );

    tx.commit()?;
    Ok(())
}

#[test]
fn test_search_index_actual_query() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture and item.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'hello world', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index the capture.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Test FTS search.
    let results = ohand_core::retrieval::index::search_index(&tx, "hello")?;
    assert_eq!(results.len(), 1, "Should find the item");
    assert_eq!(results[0].item_id, "item1");
    assert_eq!(results[0].capture_id, "cap1");
    assert_eq!(results[0].item_scope, "personal");
    assert_eq!(results[0].original_text, Some("hello world".to_string()));
    assert_eq!(results[0].corrected_text, None);
    assert_eq!(results[0].source_type, "original");

    tx.commit()?;
    Ok(())
}

#[test]
fn test_search_index_with_correction() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create a capture and item.
    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'hello world', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index original and correct it.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    tx.execute(
        "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
         VALUES ('corr1', 'item1', 0, 'text', 'hello world', 'goodbye world', '2026-01-01T00:00:00Z')",
        [],
    )?;
    ohand_core::retrieval::index::index_text_correction(&tx, "item1")?;

    // Search should find the corrected text.
    let results = ohand_core::retrieval::index::search_index(&tx, "goodbye")?;
    assert_eq!(results.len(), 1, "Should find the corrected item");
    assert_eq!(results[0].corrected_text, Some("goodbye world".to_string()));

    // Search for original should not find it (superseded by correction).
    let results_original = ohand_core::retrieval::index::search_index(&tx, "hello")?;
    assert_eq!(
        results_original.len(),
        0,
        "Original text should be superseded by correction"
    );

    tx.commit()?;
    Ok(())
}

#[test]
fn test_direct_source_fallback_agrees_with_index() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    // Create multiple captures and items.
    for i in 1..=3 {
        let item_id = format!("item{}", i);
        let cap_id = format!("cap{}", i);
        let text = format!("test content {}", i);

        tx.execute(
            "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
             timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
             VALUES (?, ?, 'personal', 'route1', '2026-01-01T00:00:00Z',
             'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
            rusqlite::params![&cap_id, &text],
        )?;
        tx.execute(
            "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
             processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 'active', 'saved', 'not_configured',
             'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![&item_id, &cap_id],
        )?;

        ohand_core::retrieval::index::index_capture(&tx, &item_id, &cap_id)?;
    }

    // Rebuild index to ensure it's complete.
    ohand_core::retrieval::index::rebuild_index(&tx)?;

    // Query via index and direct source should agree.
    let index_results = ohand_core::retrieval::index::search_index(&tx, "content")?;
    let source_results = ohand_core::retrieval::index::search_source_direct(&tx, "content")?;

    assert_eq!(
        index_results.len(),
        source_results.len(),
        "Index and source should return same number of results"
    );

    // Verify all results are present in both.
    for result in &index_results {
        let found = source_results
            .iter()
            .any(|r| r.item_id == result.item_id && r.source_type == result.source_type);
        assert!(found, "Index result not found in source fallback");
    }

    tx.commit()?;
    Ok(())
}

#[test]
fn test_idempotent_capture_indexing() -> Result<()> {
    let mut db = ohand_core::store::schema::Database::open(":memory:", Arc::new(TestClock))?;
    let tx = db.transaction()?;

    tx.execute(
        "INSERT INTO captures (capture_id, text, item_scope, route_id, capture_instant,
         timezone_id, utc_offset_minutes, locale, calendar, entry_locked, created_at)
         VALUES ('cap1', 'text', 'personal', 'route1', '2026-01-01T00:00:00Z',
         'UTC', 0, 'en_US', 'gregorian', 0, '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, lifecycle_state, save_state, sync_state,
         processing_state, transcription_state, created_at, updated_at)
         VALUES ('item1', 'cap1', 'active', 'saved', 'not_configured',
         'idle', 'no_audio', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )?;

    // Index the same capture multiple times.
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;
    ohand_core::retrieval::index::index_capture(&tx, "item1", "cap1")?;

    // Should have only one original text entry.
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index WHERE item_id = 'item1' AND text_basis = 'original'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "Idempotent indexing should not create duplicates");

    tx.commit()?;
    Ok(())
}
