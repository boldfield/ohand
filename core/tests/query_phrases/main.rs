use anyhow::Result;
use chrono::{DateTime, Offset, TimeZone, Utc};
use ohand_core::retrieval::index::sync_item_in_tx;
use ohand_core::retrieval::phrases::{parse_phrase, retrieve_phrase, ClarificationKind};
use ohand_core::retrieval::query::QueryPagination;
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock, Database};
use ohand_core::time::TimeContext;
use rusqlite::params;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::TempDir;

const REFERENCE: &str = "2026-01-15T10:30:00Z"; // a Thursday

fn context_in(timezone: &str, reference_rfc3339: &str) -> TimeContext {
    let reference_time = DateTime::parse_from_rfc3339(reference_rfc3339)
        .unwrap()
        .with_timezone(&Utc);
    let tz = chrono_tz::Tz::from_str(timezone).unwrap();
    TimeContext {
        timezone: timezone.to_string(),
        locale: "en".to_string(),
        reference_time,
        utc_offset_at_capture: tz
            .offset_from_utc_datetime(&reference_time.naive_utc())
            .fix()
            .local_minus_utc(),
        calendar: "gregorian".to_string(),
    }
}

fn utc_context() -> TimeContext {
    context_in("UTC", REFERENCE)
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(REFERENCE)
            .unwrap()
            .with_timezone(&Utc)
    }
}

fn new_db() -> Result<(TempDir, Database)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("phrases.db");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    let db = Database::open(path.to_str().unwrap(), clock)?;
    Ok((directory, db))
}

struct SeedItem<'a> {
    item_id: &'a str,
    text: &'a str,
    item_type: Option<&'a str>,
    session_topic: Option<&'a str>,
    scope: &'a str,
    capture_instant: &'a str,
}

impl<'a> SeedItem<'a> {
    fn personal(item_id: &'a str, text: &'a str, capture_instant: &'a str) -> Self {
        Self {
            item_id,
            text,
            item_type: None,
            session_topic: None,
            scope: "personal",
            capture_instant,
        }
    }

    fn topic(mut self, topic: &'a str) -> Self {
        self.session_topic = Some(topic);
        self
    }

    fn item_type(mut self, item_type: &'a str) -> Self {
        self.item_type = Some(item_type);
        self
    }

    fn scope(mut self, scope: &'a str) -> Self {
        self.scope = scope;
        self
    }
}

fn seed(db: &mut Database, item: SeedItem<'_>) -> Result<()> {
    let capture_id = format!("cap-{}", item.item_id);
    let tx = db.immediate_transaction()?;
    let capture = Capture::new(
        capture_id.clone(),
        Some(item.text.to_string()),
        None,
        item.capture_instant.to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        item.scope.to_string(),
        "test-route".to_string(),
        false,
        REFERENCE.to_string(),
        item.session_topic.map(str::to_string),
    )?;
    ohand_core::store::captures::save_capture_in_tx(&tx, &capture)?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                           save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, ?, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        params![item.item_id, &capture_id, item.item_type, REFERENCE, REFERENCE],
    )?;
    sync_item_in_tx(&tx, item.item_id)?;
    tx.commit()?;
    Ok(())
}

fn retrieved_ids(
    db: &Database,
    phrase: &str,
    context: &TimeContext,
) -> Result<Vec<(String, String)>> {
    let resolution = parse_phrase(phrase, context)?;
    let result = retrieve_phrase(db.conn(), &resolution, &QueryPagination::default())?;
    let mut hits: Vec<(String, String)> = result
        .hits
        .into_iter()
        .map(|hit| (hit.item_id, hit.current_text))
        .collect();
    hits.sort();
    Ok(hits)
}

fn ids(hits: &[(String, String)]) -> Vec<&str> {
    hits.iter().map(|(id, _)| id.as_str()).collect()
}

fn seed_session_notes(db: &mut Database) -> Result<()> {
    seed(
        db,
        SeedItem::personal(
            "therapy-after",
            "Bring up the roof worry with my counselor",
            "2026-01-14T10:00:00Z",
        )
        .topic("therapy"),
    )?;
    seed(
        db,
        SeedItem::personal(
            "therapy-before",
            "Talked about sleep last week",
            "2026-01-08T10:00:00Z",
        )
        .topic("therapy"),
    )?;
    seed(
        db,
        SeedItem::personal(
            "work-topic-after",
            "Draft the planning outline",
            "2026-01-13T09:00:00Z",
        )
        .topic("work"),
    )?;
    seed(
        db,
        SeedItem::personal(
            "no-topic-after",
            "Buy milk and eggs",
            "2026-01-14T11:00:00Z",
        ),
    )?;
    seed(
        db,
        SeedItem::personal(
            "work-scope-therapy",
            "Employer-visible therapy scheduling",
            "2026-01-14T12:00:00Z",
        )
        .topic("therapy")
        .scope("work"),
    )?;
    Ok(())
}

#[test]
fn headline_phrase_retrieves_original_words_of_any_session_topic_since_monday() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed_session_notes(&mut db)?;

    let hits = retrieved_ids(&db, "private session notes since monday", &utc_context())?;

    assert_eq!(ids(&hits), vec!["therapy-after", "work-topic-after"]);
    let therapy = hits.iter().find(|(id, _)| id == "therapy-after").unwrap();
    assert_eq!(therapy.1, "Bring up the roof worry with my counselor");
    Ok(())
}

#[test]
fn named_topic_is_matched_and_other_topics_scopes_and_dates_are_excluded() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed_session_notes(&mut db)?;

    let hits = retrieved_ids(&db, "private therapy notes since monday", &utc_context())?;
    assert_eq!(ids(&hits), vec!["therapy-after"]);

    let without_date = retrieved_ids(&db, "private therapy notes", &utc_context())?;
    assert_eq!(ids(&without_date), vec!["therapy-after", "therapy-before"]);
    Ok(())
}

#[test]
fn topic_matches_stored_capitalization() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed(
        &mut db,
        SeedItem::personal(
            "capitalized",
            "Ask about boundaries",
            "2026-01-14T10:00:00Z",
        )
        .topic("Therapy"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("lowercase", "Ask about homework", "2026-01-14T10:00:00Z")
            .topic("therapy"),
    )?;

    for phrase in ["Private Therapy notes", "private therapy notes"] {
        let hits = retrieved_ids(&db, phrase, &utc_context())?;
        assert_eq!(ids(&hits), vec!["capitalized", "lowercase"], "{phrase}");
    }
    Ok(())
}

#[test]
fn topic_matches_any_stored_casing_and_surrounding_whitespace() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    for (item_id, topic) in [
        ("upper", "THERAPY"),
        ("padded", " therapy "),
        ("mixed", "tHeRaPy"),
    ] {
        seed(
            &mut db,
            SeedItem::personal(item_id, "Ask about boundaries", "2026-01-14T10:00:00Z")
                .topic(topic),
        )?;
    }
    seed(
        &mut db,
        SeedItem::personal("other", "Ask about boundaries", "2026-01-14T10:00:00Z").topic("work"),
    )?;

    for phrase in [
        "private therapy notes",
        "PRIVATE THERAPY NOTES since monday",
        "private  therapy   session notes",
    ] {
        let hits = retrieved_ids(&db, phrase, &utc_context())?;
        assert_eq!(ids(&hits), vec!["mixed", "padded", "upper"], "{phrase}");
    }
    Ok(())
}

#[test]
fn spoken_broad_intentions_retrieve_only_that_type() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed(
        &mut db,
        SeedItem::personal("broad-new", "Be more present", "2026-01-14T10:00:00Z")
            .item_type("broad_intention"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("broad-old", "Read more", "2026-01-05T10:00:00Z")
            .item_type("broad_intention"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("action-new", "Call the plumber", "2026-01-14T10:00:00Z")
            .item_type("action"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("note-new", "Plumber number", "2026-01-14T10:00:00Z").item_type("note"),
    )?;

    for phrase in [
        "broad intentions since monday",
        "broad intention notes since monday",
        "Broad Intentions since monday",
        "broad_intentions since monday",
    ] {
        let hits = retrieved_ids(&db, phrase, &utc_context())?;
        assert_eq!(ids(&hits), vec!["broad-new"], "{phrase}");
    }
    let all = retrieved_ids(&db, "broad intentions", &utc_context())?;
    assert_eq!(ids(&all), vec!["broad-new", "broad-old"]);
    Ok(())
}

#[test]
fn type_and_date_combine() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed(
        &mut db,
        SeedItem::personal("action-new", "Call the plumber", "2026-01-14T10:00:00Z")
            .item_type("action"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("action-old", "Renew the passport", "2026-01-05T10:00:00Z")
            .item_type("action"),
    )?;
    seed(
        &mut db,
        SeedItem::personal("idea-new", "Try a standing desk", "2026-01-14T10:00:00Z")
            .item_type("idea"),
    )?;

    let hits = retrieved_ids(&db, "action notes since friday", &utc_context())?;
    assert_eq!(ids(&hits), vec!["action-new"]);

    let ideas = retrieved_ids(&db, "ideas since 2026-01-01", &utc_context())?;
    assert_eq!(ids(&ideas), vec!["idea-new"]);
    Ok(())
}

#[test]
fn date_bound_is_local_midnight_in_the_context_timezone() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    // 2026-01-14 00:00 America/Los_Angeles is 08:00Z.
    seed(
        &mut db,
        SeedItem::personal(
            "just-before",
            "Before local midnight",
            "2026-01-14T07:59:59Z",
        ),
    )?;
    seed(
        &mut db,
        SeedItem::personal("at-midnight", "At local midnight", "2026-01-14T08:00:00Z"),
    )?;
    let los_angeles = context_in("America/Los_Angeles", "2026-01-15T10:30:00Z");

    let hits = retrieved_ids(&db, "notes since 2026-01-14", &los_angeles)?;
    assert_eq!(ids(&hits), vec!["at-midnight"]);

    let utc_hits = retrieved_ids(&db, "notes since 2026-01-14", &utc_context())?;
    assert_eq!(
        ids(&utc_hits),
        vec!["at-midnight", "just-before"],
        "UTC midnight is earlier, so the same query includes both"
    );
    Ok(())
}

#[test]
fn today_near_utc_midnight_uses_the_local_day() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    // Local time is 2026-01-14 19:30 in Los Angeles; local today began at 2026-01-14T08:00Z.
    seed(
        &mut db,
        SeedItem::personal(
            "earlier-today",
            "Local morning note",
            "2026-01-14T16:00:00Z",
        ),
    )?;
    seed(
        &mut db,
        SeedItem::personal(
            "yesterday",
            "Local previous day note",
            "2026-01-13T20:00:00Z",
        ),
    )?;
    let context = context_in("America/Los_Angeles", "2026-01-15T03:30:00Z");

    let today = retrieved_ids(&db, "notes since today", &context)?;
    assert_eq!(ids(&today), vec!["earlier-today"]);

    let since_yesterday = retrieved_ids(&db, "notes since yesterday", &context)?;
    assert_eq!(ids(&since_yesterday), vec!["earlier-today", "yesterday"]);
    Ok(())
}

#[test]
fn unsupported_phrasing_falls_back_to_literal_search_of_the_original_words() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed(
        &mut db,
        SeedItem::personal(
            "roof",
            "something about the roof leak needs a plumber",
            "2026-01-14T10:00:00Z",
        ),
    )?;
    seed(
        &mut db,
        SeedItem::personal("other", "Buy milk", "2026-01-14T10:00:00Z"),
    )?;

    let resolution = parse_phrase("something about the roof", &utc_context())?;
    assert!(resolution.filter.is_none());
    assert!(resolution.clarification_needed.is_none());
    assert_eq!(resolution.fallback_search_text, "something about the roof");

    let hits = retrieved_ids(&db, "something about the roof", &utc_context())?;
    assert_eq!(ids(&hits), vec!["roof"]);
    assert_eq!(hits[0].1, "something about the roof leak needs a plumber");
    Ok(())
}

#[test]
fn uncertain_date_clarifies_and_literal_fallback_uses_the_parsers_text() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed(
        &mut db,
        SeedItem::personal(
            "literal",
            "Reminder: notes since last week were messy",
            "2026-01-14T10:00:00Z",
        ),
    )?;
    seed(
        &mut db,
        SeedItem::personal("unrelated", "Buy milk", "2026-01-14T10:00:00Z"),
    )?;

    let context = utc_context();
    let resolution = parse_phrase("notes since last week", &context)?;
    assert!(resolution.filter.is_none());
    assert_eq!(
        resolution.clarification_needed,
        Some(ClarificationKind::UnrecognizedDate {
            phrase: "last week".to_string()
        })
    );
    assert_eq!(resolution.fallback_search_text, "notes since last week");

    let hits = retrieved_ids(&db, "notes since last week", &context)?;
    assert_eq!(ids(&hits), vec!["literal"]);
    Ok(())
}

#[test]
fn ambiguous_and_future_dates_do_not_commit_a_bound() -> Result<()> {
    let context = utc_context();

    let same_weekday = parse_phrase("notes since thursday", &context)?;
    assert!(same_weekday.filter.is_none());
    assert!(matches!(
        same_weekday.clarification_needed,
        Some(ClarificationKind::AmbiguousDate { ref candidate_dates, .. })
            if candidate_dates == &["2026-01-15".to_string(), "2026-01-08".to_string()]
    ));

    let future = parse_phrase("private session notes since tomorrow", &context)?;
    assert!(future.filter.is_none());
    assert!(matches!(
        future.clarification_needed,
        Some(ClarificationKind::FutureSinceBound { .. })
    ));

    let fold = context_in("America/New_York", "2025-11-10T12:00:00Z");
    let ambiguous = parse_phrase("notes since 2025-11-02 01:30:00", &fold)?;
    assert!(ambiguous.filter.is_none());
    assert!(matches!(
        ambiguous.clarification_needed,
        Some(ClarificationKind::AmbiguousTime { .. })
    ));
    Ok(())
}

#[test]
fn invalid_time_context_is_surfaced_instead_of_dropping_the_date() {
    let mut bad_timezone = utc_context();
    bad_timezone.timezone = "Not/A_Zone".to_string();
    let mut bad_calendar = utc_context();
    bad_calendar.calendar = "hebrew".to_string();
    let mut bad_offset = context_in("America/Los_Angeles", REFERENCE);
    bad_offset.utc_offset_at_capture = 0;

    for context in [&bad_timezone, &bad_calendar, &bad_offset] {
        for phrase in [
            "action notes since today",
            "action notes since yesterday",
            "action notes since monday",
            "action notes since 2026-01-10",
        ] {
            assert!(parse_phrase(phrase, context).is_err(), "{phrase}");
        }
    }
}

#[test]
fn parsing_and_retrieval_create_no_jobs_events_or_provider_requests() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    seed_session_notes(&mut db)?;

    let table_names: Vec<String> = {
        let mut statement = db.conn().prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'search_index%'
              ORDER BY name",
        )?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        names
    };
    assert!(table_names.iter().any(|name| name == "jobs"));
    let snapshot = |db: &Database| -> Result<Vec<(String, i64)>> {
        table_names
            .iter()
            .map(|name| {
                let count: i64 = db.conn().query_row(
                    &format!("SELECT COUNT(*) FROM \"{name}\""),
                    [],
                    |row| row.get(0),
                )?;
                Ok((name.clone(), count))
            })
            .collect()
    };

    let before = snapshot(&db)?;
    for phrase in [
        "private session notes since monday",
        "private therapy notes",
        "something about the roof",
    ] {
        retrieved_ids(&db, phrase, &utc_context())?;
    }
    assert_eq!(snapshot(&db)?, before);
    Ok(())
}

#[test]
fn original_phrase_is_preserved_and_case_is_ignored_for_keywords() -> Result<()> {
    let original = "  Private Session Notes SINCE Monday  ";
    let resolution = parse_phrase(original, &utc_context())?;
    assert_eq!(resolution.original_phrase, original);
    assert_eq!(resolution.fallback_search_text, original);
    let filter = resolution.filter.expect("keywords are case-insensitive");
    assert!(filter.require_session_topic);
    assert_eq!(
        filter.captured_after.as_deref(),
        Some("2026-01-12T00:00:00+00:00")
    );
    Ok(())
}

#[test]
fn malformed_ordinal_suffix_is_unrecognized_not_a_committed_date() -> Result<()> {
    let context = utc_context();
    for phrase in [
        "notes since jan 10garbage",
        "notes since jan 10xyz 2025",
        "notes since jan 11st",
        "notes since jan 12nd",
        "notes since jan 13rd",
        "notes since jan 1th",
        "notes since jan 2st",
        "notes since jan 3nd",
        "notes since jan 22rd",
        "notes since jan 31th",
        "notes since jan +5",
    ] {
        let resolution = parse_phrase(phrase, &context)?;
        assert!(resolution.filter.is_none(), "{phrase}");
        assert!(
            matches!(
                resolution.clarification_needed,
                Some(ClarificationKind::UnrecognizedDate { .. })
            ),
            "{phrase}"
        );
        assert_eq!(resolution.fallback_search_text, phrase);
    }
    for (phrase, expected) in [
        ("notes since jan 10", "2026-01-10T00:00:00+00:00"),
        ("notes since jan 1st", "2026-01-01T00:00:00+00:00"),
        ("notes since jan 2nd", "2026-01-02T00:00:00+00:00"),
        ("notes since jan 3rd", "2026-01-03T00:00:00+00:00"),
        ("notes since jan 10th", "2026-01-10T00:00:00+00:00"),
        ("notes since jan 11th", "2026-01-11T00:00:00+00:00"),
        ("notes since jan 12th", "2026-01-12T00:00:00+00:00"),
        ("notes since jan 13th", "2026-01-13T00:00:00+00:00"),
        ("notes since jan 21st", "2025-01-21T00:00:00+00:00"),
        ("notes since jan 22nd", "2025-01-22T00:00:00+00:00"),
        ("notes since jan 23rd", "2025-01-23T00:00:00+00:00"),
        ("notes since jan 31st", "2025-01-31T00:00:00+00:00"),
    ] {
        let resolution = parse_phrase(phrase, &context)?;
        let filter = resolution.filter.expect(phrase);
        assert_eq!(filter.captured_after.as_deref(), Some(expected), "{phrase}");
    }
    Ok(())
}

#[test]
fn fully_skipped_local_date_withholds_the_bound_instead_of_using_the_next_day() -> Result<()> {
    // Pacific/Apia skipped 2011-12-30 entirely when it crossed the date line.
    let context = context_in("Pacific/Apia", "2012-01-01T12:00:00Z");
    let skipped = parse_phrase("notes since 2011-12-30", &context)?;
    assert!(skipped.filter.is_none());
    assert!(matches!(
        skipped.clarification_needed,
        Some(ClarificationKind::AmbiguousTime { .. })
    ));
    assert_eq!(skipped.fallback_search_text, "notes since 2011-12-30");

    let existing = parse_phrase("notes since 2011-12-31", &context)?;
    let filter = existing.filter.expect("2011-12-31 exists locally");
    assert!(existing.clarification_needed.is_none());
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn filter_only_retrieval_pages_newest_capture_first_across_precision_and_offsets() -> Result<()> {
    let (_directory, mut db) = new_db()?;
    // Text order of these RFC3339 strings is not chronological order.
    for (item_id, capture_instant) in [
        ("whole-second", "2026-01-15T10:30:00Z"),
        ("fractional", "2026-01-15T10:30:00.900Z"),
        ("offset-newest", "2026-01-15T05:30:00.950-05:00"),
        ("offset-oldest", "2026-01-15T11:00:00+02:00"),
    ] {
        seed(
            &mut db,
            SeedItem::personal(item_id, "synthetic note", capture_instant),
        )?;
    }
    let resolution = parse_phrase("notes since today", &utc_context())?;
    assert!(resolution.filter.is_some());

    let expected_order = [
        "offset-newest",
        "fractional",
        "whole-second",
        "offset-oldest",
    ];
    let all = retrieve_phrase(db.conn(), &resolution, &QueryPagination::default())?;
    let all_ids: Vec<&str> = all.hits.iter().map(|hit| hit.item_id.as_str()).collect();
    assert_eq!(all_ids, expected_order);

    for (offset, expected_id) in expected_order.iter().enumerate() {
        let page = retrieve_phrase(
            db.conn(),
            &resolution,
            &QueryPagination { limit: 1, offset },
        )?;
        let page_ids: Vec<&str> = page.hits.iter().map(|hit| hit.item_id.as_str()).collect();
        assert_eq!(page_ids, [*expected_id], "page at offset {offset}");
        assert_eq!(page.total_accessible, expected_order.len());
    }
    Ok(())
}
