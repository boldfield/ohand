use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::sync::Arc;

use ohand_core::retrieval::index::{
    rebuild_index, remove_item_from_index, search_index, search_source_direct, sync_item_in_tx,
    IndexChange, MatchedText, SearchHit, TextBasis,
};
use ohand_core::store::captures::{save_capture_in_tx, Capture};
use ohand_core::store::events::{
    delete_events_for_item, save_event, save_event_in_tx, Correction, CorrectionKind, Event,
    EventPayload, EventType, ItemScope,
};
use ohand_core::store::schema::{Clock, Database};

const BOTH_SCOPES: [ItemScope; 2] = [ItemScope::Personal, ItemScope::Work];

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_search_index_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn open_db(path: &str) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    Database::open(path, clock)
}

fn new_db(label: &str) -> Result<Database> {
    open_db(&temp_db_path(label))
}

/// Save a capture through the real capture API and attach an item to it. There is no item
/// creation API yet, so the item row is written directly, then indexed through the R01 hook.
fn add_item(
    db: &mut Database,
    item_id: &str,
    text: Option<&str>,
    scope: &str,
    audio_reference: Option<&str>,
) -> Result<String> {
    let capture_id = format!("cap-{item_id}");
    let tx = db.immediate_transaction()?;
    let capture = Capture::new(
        capture_id.clone(),
        text.map(str::to_string),
        audio_reference.map(str::to_string),
        "2026-01-15T10:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        "route-1".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        None,
    )?;
    save_capture_in_tx(&tx, &capture)?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state,
                            processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        rusqlite::params![
            item_id,
            &capture_id,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z"
        ],
    )?;
    sync_item_in_tx(&tx, item_id)?;
    tx.commit()?;
    Ok(capture_id)
}

fn text_correction_event(
    event_id: &str,
    item_id: &str,
    revision: i32,
    old_value: Option<&str>,
    new_value: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: old_value.map(str::to_string),
            new_value: new_value.to_string(),
        }),
        "2026-01-15T11:00:00Z".to_string(),
    )
    .unwrap()
}

fn scope_correction_event(
    event_id: &str,
    item_id: &str,
    revision: i32,
    from: &str,
    to: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Scope,
            old_value: Some(from.to_string()),
            new_value: to.to_string(),
        }),
        "2026-01-15T11:00:00Z".to_string(),
    )
    .unwrap()
}

type IndexRow = (String, Option<String>, Option<String>, String);

fn index_rows(conn: &Connection) -> Result<Vec<IndexRow>> {
    let mut statement = conn.prepare(
        "SELECT item_id, original_text, current_text, text_basis FROM search_index
         ORDER BY item_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn hit_ids(hits: &[SearchHit]) -> Vec<String> {
    let mut ids: Vec<String> = hits.iter().map(|hit| hit.item_id.clone()).collect();
    ids.sort();
    ids
}

fn rebuild(db: &mut Database) -> Result<usize> {
    let tx = db.immediate_transaction()?;
    let count = rebuild_index(&tx)?;
    tx.commit()?;
    Ok(count)
}

#[test]
fn indexed_item_is_found_with_source_attribution() -> Result<()> {
    let mut db = new_db("attribution")?;
    let capture_id = add_item(&mut db, "item-1", Some("Buy fresh milk"), "personal", None)?;

    let hits = search_index(db.conn(), "milk", &BOTH_SCOPES)?;
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.item_id, "item-1");
    assert_eq!(hit.capture_id, capture_id);
    assert_eq!(hit.scope, ItemScope::Personal);
    assert_eq!(hit.route_id, "route-1");
    assert_eq!(hit.lifecycle_state, "active");
    assert_eq!(hit.current_text, "Buy fresh milk");
    assert_eq!(hit.original_text.as_deref(), Some("Buy fresh milk"));
    assert_eq!(hit.text_basis, TextBasis::Original);
    assert_eq!(hit.matched, MatchedText::Current);
    Ok(())
}

#[test]
fn text_correction_through_save_event_updates_index_transactionally() -> Result<()> {
    let mut db = new_db("correction_hook")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;

    save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 0, Some("buy milk"), "buy eggs"),
        0,
    )?;
    save_event(
        &mut db,
        &text_correction_event("e2", "item-1", 1, Some("buy eggs"), "buy bread"),
        1,
    )?;

    // Only the latest correction is searchable; the superseded one is gone.
    assert!(search_index(db.conn(), "eggs", &BOTH_SCOPES)?.is_empty());
    let bread = search_index(db.conn(), "bread", &BOTH_SCOPES)?;
    assert_eq!(bread.len(), 1);
    assert_eq!(bread[0].current_text, "buy bread");
    assert_eq!(bread[0].text_basis, TextBasis::Corrected);
    assert_eq!(bread[0].matched, MatchedText::Current);

    // The original stays searchable but is labelled as superseded.
    let milk = search_index(db.conn(), "milk", &BOTH_SCOPES)?;
    assert_eq!(milk.len(), 1);
    assert_eq!(milk[0].matched, MatchedText::SupersededOriginal);
    assert_eq!(milk[0].current_text, "buy bread");
    assert_eq!(milk[0].original_text.as_deref(), Some("buy milk"));

    // One row per item, identical to a rebuild.
    let incremental = index_rows(db.conn())?;
    assert_eq!(incremental.len(), 1);
    rebuild(&mut db)?;
    assert_eq!(index_rows(db.conn())?, incremental);
    Ok(())
}

#[test]
fn uncommitted_correction_leaves_index_untouched() -> Result<()> {
    let mut db = new_db("rollback")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    let before = index_rows(db.conn())?;

    {
        let tx = db.immediate_transaction()?;
        save_event_in_tx(
            &tx,
            &text_correction_event("e1", "item-1", 0, Some("buy milk"), "buy eggs"),
            0,
        )?;
        let inside: i64 = tx.query_row(
            "SELECT COUNT(*) FROM search_index WHERE current_text = 'buy eggs'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(inside, 1, "the index changes inside the transaction");
        // Dropped without commit: rolls back source and index together.
    }

    assert_eq!(index_rows(db.conn())?, before);
    assert!(search_index(db.conn(), "eggs", &BOTH_SCOPES)?.is_empty());
    assert_eq!(search_index(db.conn(), "milk", &BOTH_SCOPES)?.len(), 1);
    Ok(())
}

#[test]
fn rejected_correction_does_not_touch_index() -> Result<()> {
    let mut db = new_db("rejected")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    let before = index_rows(db.conn())?;

    // Stale revision and wrong old value are both refused before any write.
    assert!(save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 5, Some("buy milk"), "buy eggs"),
        5
    )
    .is_err());
    assert!(save_event(
        &mut db,
        &text_correction_event("e2", "item-1", 0, Some("wrong"), "buy eggs"),
        0
    )
    .is_err());
    assert_eq!(index_rows(db.conn())?, before);
    Ok(())
}

#[test]
fn committed_index_survives_reopen_and_matches_rebuild() -> Result<()> {
    let path = temp_db_path("reopen");
    {
        let mut db = open_db(&path)?;
        add_item(
            &mut db,
            "item-1",
            Some("call the dentist"),
            "personal",
            None,
        )?;
        add_item(&mut db, "item-2", Some("quarterly budget"), "work", None)?;
        save_event(
            &mut db,
            &text_correction_event(
                "e1",
                "item-1",
                0,
                Some("call the dentist"),
                "call the plumber",
            ),
            0,
        )?;
    }
    let mut db = open_db(&path)?;
    let persisted = index_rows(db.conn())?;
    assert_eq!(persisted.len(), 2);
    assert_eq!(search_index(db.conn(), "plumber", &BOTH_SCOPES)?.len(), 1);

    rebuild(&mut db)?;
    assert_eq!(index_rows(db.conn())?, persisted);
    Ok(())
}

#[test]
fn event_retry_is_idempotent_for_the_index() -> Result<()> {
    let mut db = new_db("retry")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    let event = text_correction_event("e1", "item-1", 0, Some("buy milk"), "buy eggs");
    save_event(&mut db, &event, 0)?;
    let after_first = index_rows(db.conn())?;
    save_event(&mut db, &event, 0)?;
    assert_eq!(index_rows(db.conn())?, after_first);
    assert_eq!(after_first.len(), 1);
    Ok(())
}

#[test]
fn sync_is_idempotent() -> Result<()> {
    let mut db = new_db("sync_idempotent")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    let before = index_rows(db.conn())?;
    let tx = db.immediate_transaction()?;
    for _ in 0..3 {
        assert_eq!(sync_item_in_tx(&tx, "item-1")?, IndexChange::Indexed);
    }
    assert_eq!(sync_item_in_tx(&tx, "missing-item")?, IndexChange::Removed);
    tx.commit()?;
    assert_eq!(index_rows(db.conn())?, before);
    Ok(())
}

#[test]
fn deleted_item_is_removed_and_cannot_be_resurrected_by_retry() -> Result<()> {
    let mut db = new_db("deleted")?;
    add_item(&mut db, "item-1", Some("secret plan"), "personal", None)?;
    add_item(&mut db, "item-2", Some("public plan"), "personal", None)?;

    let tx = db.immediate_transaction()?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = 'item-1'",
        [],
    )?;
    // Deletion hook plus a retried index hook and a retried correction replay.
    assert_eq!(remove_item_from_index(&tx, "item-1")?, 1);
    assert_eq!(sync_item_in_tx(&tx, "item-1")?, IndexChange::Removed);
    tx.commit()?;

    assert!(search_index(db.conn(), "secret", &BOTH_SCOPES)?.is_empty());
    assert!(search_source_direct(db.conn(), "secret", &BOTH_SCOPES)?.is_empty());
    assert_eq!(index_rows(db.conn())?.len(), 1);

    // A deleted item refuses corrections, so a racing correction cannot restore text.
    assert!(save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 0, Some("secret plan"), "secret again"),
        0
    )
    .is_err());
    assert!(search_index(db.conn(), "again", &BOTH_SCOPES)?.is_empty());
    rebuild(&mut db)?;
    assert!(search_index(db.conn(), "secret", &BOTH_SCOPES)?.is_empty());
    Ok(())
}

#[test]
fn stale_or_invented_index_rows_are_neither_returned_nor_survive_rebuild() -> Result<()> {
    let mut db = new_db("invented")?;
    add_item(&mut db, "item-1", Some("real note"), "personal", None)?;
    add_item(&mut db, "item-2", Some("doomed note"), "personal", None)?;

    // Simulate a crash that left an orphan row and a stale row for a deleted item.
    db.conn().execute(
        "INSERT INTO search_index (item_id, original_text, current_text, text_basis)
         VALUES ('ghost', 'ghost note', 'ghost note', 'original')",
        [],
    )?;
    db.conn().execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = 'item-2'",
        [],
    )?;

    // Query-time joins on authoritative state hide both rows even before a rebuild.
    assert_eq!(
        hit_ids(&search_index(db.conn(), "note", &BOTH_SCOPES)?),
        vec!["item-1"]
    );

    assert_eq!(rebuild(&mut db)?, 1);
    let rows = index_rows(db.conn())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "item-1");
    Ok(())
}

#[test]
fn scope_correction_changes_effective_scope_and_filter() -> Result<()> {
    let mut db = new_db("scope")?;
    add_item(&mut db, "item-1", Some("roadmap review"), "personal", None)?;
    add_item(&mut db, "item-2", Some("roadmap lunch"), "personal", None)?;
    save_event(
        &mut db,
        &scope_correction_event("e1", "item-1", 0, "personal", "work"),
        0,
    )?;

    let all = search_index(db.conn(), "roadmap", &BOTH_SCOPES)?;
    let scope_of = |id: &str| all.iter().find(|hit| hit.item_id == id).unwrap().scope;
    assert_eq!(scope_of("item-1"), ItemScope::Work);
    assert_eq!(scope_of("item-2"), ItemScope::Personal);

    let work_only = search_index(db.conn(), "roadmap", &[ItemScope::Work])?;
    assert_eq!(hit_ids(&work_only), vec!["item-1"]);
    let personal_only = search_index(db.conn(), "roadmap", &[ItemScope::Personal])?;
    assert_eq!(hit_ids(&personal_only), vec!["item-2"]);
    assert!(search_index(db.conn(), "roadmap", &[])?.is_empty());

    // The fallback agrees under every scope selection.
    for scopes in [
        &BOTH_SCOPES[..],
        &[ItemScope::Work][..],
        &[ItemScope::Personal][..],
    ] {
        assert_eq!(
            hit_ids(&search_index(db.conn(), "roadmap", scopes)?),
            hit_ids(&search_source_direct(db.conn(), "roadmap", scopes)?)
        );
    }
    Ok(())
}

#[test]
fn ranking_returns_best_match_first() -> Result<()> {
    let mut db = new_db("ranking")?;
    add_item(
        &mut db,
        "item-a",
        Some("one two three four five six seven eight nine ten milk eleven twelve thirteen"),
        "personal",
        None,
    )?;
    add_item(&mut db, "item-b", Some("milk milk milk"), "personal", None)?;
    let hits = search_index(db.conn(), "milk", &BOTH_SCOPES)?;
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].item_id, "item-b");
    assert_eq!(hits[1].item_id, "item-a");
    Ok(())
}

#[test]
fn model_derived_text_is_never_indexed() -> Result<()> {
    let mut db = new_db("derived")?;
    add_item(
        &mut db,
        "item-1",
        Some("dentist on friday"),
        "personal",
        None,
    )?;
    db.conn().execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version,
                                text_basis_kind, applied_state, reminder_proposal,
                                session_topic_proposal, abstained, created_at)
         VALUES ('p1', 'item-1', 'cap-item-1', 0, 1, 'original', 'applied',
                 'zebrasummary reminder', 'zebrasummary topic', 0, '2026-01-15T10:30:00Z')",
        [],
    )?;
    let tx = db.immediate_transaction()?;
    sync_item_in_tx(&tx, "item-1")?;
    tx.commit()?;
    assert!(search_index(db.conn(), "zebrasummary", &BOTH_SCOPES)?.is_empty());
    rebuild(&mut db)?;
    assert!(search_index(db.conn(), "zebrasummary", &BOTH_SCOPES)?.is_empty());
    assert_eq!(search_index(db.conn(), "dentist", &BOTH_SCOPES)?.len(), 1);
    Ok(())
}

#[test]
fn audio_only_capture_is_indexed_only_once_a_transcript_is_corrected_in() -> Result<()> {
    let mut db = new_db("audio")?;
    add_item(&mut db, "item-1", None, "personal", Some("audio/ref-1"))?;
    assert!(index_rows(db.conn())?.is_empty());

    save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 0, None, "remember the umbrella"),
        0,
    )?;
    let hits = search_index(db.conn(), "umbrella", &BOTH_SCOPES)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].original_text, None);
    assert_eq!(hits[0].text_basis, TextBasis::Corrected);
    assert_eq!(hits[0].matched, MatchedText::Current);
    let incremental = index_rows(db.conn())?;
    rebuild(&mut db)?;
    assert_eq!(index_rows(db.conn())?, incremental);
    Ok(())
}

#[test]
fn correction_to_empty_text_clears_prior_corrected_text() -> Result<()> {
    let mut db = new_db("empty_correction")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 0, Some("buy milk"), "buy eggs"),
        0,
    )?;
    save_event(
        &mut db,
        &text_correction_event("e2", "item-1", 1, Some("buy eggs"), ""),
        1,
    )?;
    assert!(search_index(db.conn(), "eggs", &BOTH_SCOPES)?.is_empty());
    let incremental = index_rows(db.conn())?;
    rebuild(&mut db)?;
    assert_eq!(index_rows(db.conn())?, incremental);
    Ok(())
}

#[test]
fn removing_corrections_resyncs_the_index() -> Result<()> {
    let mut db = new_db("delete_events")?;
    add_item(&mut db, "item-1", Some("buy milk"), "personal", None)?;
    save_event(
        &mut db,
        &text_correction_event("e1", "item-1", 0, Some("buy milk"), "buy eggs"),
        0,
    )?;
    let tx = db.immediate_transaction()?;
    delete_events_for_item(&tx, "item-1")?;
    tx.commit()?;
    assert!(search_index(db.conn(), "eggs", &BOTH_SCOPES)?.is_empty());
    let hits = search_index(db.conn(), "milk", &BOTH_SCOPES)?;
    assert_eq!(hits[0].text_basis, TextBasis::Original);
    Ok(())
}

#[test]
fn query_edge_cases_agree_between_index_and_direct_fallback() -> Result<()> {
    let mut db = new_db("parity_queries")?;
    add_item(
        &mut db,
        "item-1",
        Some("buy groceries today"),
        "personal",
        None,
    )?;
    add_item(
        &mut db,
        "item-2",
        Some("Quarterly Budget: 100% done!"),
        "work",
        None,
    )?;
    add_item(
        &mut db,
        "item-3",
        Some("say \"hello\" or goodbye"),
        "personal",
        None,
    )?;
    add_item(
        &mut db,
        "item-4",
        Some("wild* card NEAR(a b) AND stuff"),
        "work",
        None,
    )?;
    add_item(
        &mut db,
        "item-5",
        Some("naïve café résumé"),
        "personal",
        None,
    )?;
    save_event(
        &mut db,
        &text_correction_event(
            "e1",
            "item-1",
            0,
            Some("buy groceries today"),
            "buy bread tomorrow",
        ),
        0,
    )?;

    let queries = [
        "grocer",
        "groceries",
        "GROCER",
        "bread",
        "today",
        "buy OR x",
        "buy AND bread",
        "\"",
        "\"hello\"",
        "hello goodbye",
        "goodbye hello",
        "100%",
        "%",
        "_",
        "wild*",
        "wild",
        "NEAR(a b)",
        "NOT",
        "-",
        "",
        "   ",
        "budget:",
        "original",
        "corrected",
        "current_text:buy",
        "cafe",
        "café",
        "NAIVE",
        "résumé",
        "(",
        "tomorrow buy",
        "quarterly budget",
    ];
    for query in queries {
        for scopes in [
            &BOTH_SCOPES[..],
            &[ItemScope::Work][..],
            &[ItemScope::Personal][..],
        ] {
            let indexed = hit_ids(&search_index(db.conn(), query, scopes)?);
            let direct = hit_ids(&search_source_direct(db.conn(), query, scopes)?);
            assert_eq!(indexed, direct, "query {query:?}, scopes {scopes:?}");
        }
    }

    assert_eq!(
        hit_ids(&search_index(db.conn(), "grocer", &BOTH_SCOPES)?),
        vec!["item-1"]
    );
    assert_eq!(
        hit_ids(&search_index(db.conn(), "buy OR x", &BOTH_SCOPES)?),
        Vec::<String>::new()
    );
    assert_eq!(
        hit_ids(&search_index(db.conn(), "hello goodbye", &BOTH_SCOPES)?),
        vec!["item-3"]
    );
    assert!(search_index(db.conn(), "\"", &BOTH_SCOPES)?.is_empty());
    assert!(search_index(db.conn(), "original", &BOTH_SCOPES)?.is_empty());
    assert!(search_index(db.conn(), "", &BOTH_SCOPES)?.is_empty());
    assert_eq!(
        hit_ids(&search_index(db.conn(), "wild*", &BOTH_SCOPES)?),
        vec!["item-4"]
    );
    // Superseded original text is still searchable but flagged.
    let hits = search_index(db.conn(), "groceries", &BOTH_SCOPES)?;
    assert_eq!(hits[0].matched, MatchedText::SupersededOriginal);
    Ok(())
}

/// Deterministic pseudo-random generator so the sequence test is reproducible.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

#[test]
fn incremental_index_rebuild_and_direct_fallback_agree_after_random_hook_sequences() -> Result<()> {
    let words = [
        "milk", "eggs", "bread", "plan", "budget", "dentist", "garden", "call",
    ];
    for seed in 0..6u64 {
        let mut db = new_db(&format!("sequence_{seed}"))?;
        let mut rng = Lcg(seed + 1);
        let mut live: Vec<(String, i32, String)> = Vec::new();
        let mut next_event = 0;

        for step in 0..40 {
            match rng.next(5) {
                0 | 1 if live.len() < 8 => {
                    let item_id = format!("item-{seed}-{step}");
                    let text = format!(
                        "{} {} {}",
                        words[rng.next(8) as usize],
                        words[rng.next(8) as usize],
                        step
                    );
                    let scope = if rng.next(2) == 0 { "personal" } else { "work" };
                    add_item(&mut db, &item_id, Some(&text), scope, None)?;
                    live.push((item_id, 0, text));
                }
                2 | 3 if !live.is_empty() => {
                    let index = rng.next(live.len() as u64) as usize;
                    let (item_id, revision, current) = live[index].clone();
                    let new_text = format!("{} {}", words[rng.next(8) as usize], step);
                    next_event += 1;
                    save_event(
                        &mut db,
                        &text_correction_event(
                            &format!("ev-{next_event}"),
                            &item_id,
                            revision,
                            Some(&current),
                            &new_text,
                        ),
                        revision,
                    )?;
                    live[index] = (item_id, revision + 1, new_text);
                }
                _ if !live.is_empty() => {
                    let index = rng.next(live.len() as u64) as usize;
                    let (item_id, _, _) = live.remove(index);
                    let tx = db.immediate_transaction()?;
                    tx.execute(
                        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
                        [&item_id],
                    )?;
                    remove_item_from_index(&tx, &item_id)?;
                    tx.commit()?;
                }
                _ => {}
            }

            let incremental = index_rows(db.conn())?;
            let queries: Vec<String> = words.iter().map(|word| word.to_string()).collect();
            let before: Vec<Vec<String>> = queries
                .iter()
                .map(|query| Ok(hit_ids(&search_index(db.conn(), query, &BOTH_SCOPES)?)))
                .collect::<Result<_>>()?;
            let direct: Vec<Vec<String>> = queries
                .iter()
                .map(|query| {
                    Ok(hit_ids(&search_source_direct(
                        db.conn(),
                        query,
                        &BOTH_SCOPES,
                    )?))
                })
                .collect::<Result<_>>()?;
            assert_eq!(before, direct, "seed {seed} step {step}");
            assert_eq!(incremental.len(), live.len(), "seed {seed} step {step}");

            if step % 10 == 9 {
                rebuild(&mut db)?;
                assert_eq!(
                    index_rows(db.conn())?,
                    incremental,
                    "seed {seed} step {step}"
                );
            }
        }
    }
    Ok(())
}
