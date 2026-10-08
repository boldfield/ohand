use anyhow::Result;
use chrono::{DateTime, Utc};
use ohand_core::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use ohand_core::interpretation::apply::apply_proposal;
use ohand_core::interpretation::contracts::{
    Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
};
use ohand_core::providers::contracts::TextBasis;
use ohand_core::store::events::ItemType;
use ohand_core::store::schema::{Clock, Database};
use std::sync::Arc;

const ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
const CAPTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
const PROPOSAL_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
const JOB_ID: &str = "550e8400-e29b-41d4-a716-446655440004";
const TEXT: &str = "Call the dentist tomorrow at 9am";

struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2025-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }
}

fn setup_database() -> Result<Database> {
    let tmpdir = std::env::temp_dir();
    let path = format!(
        "{}/test_proposal_app_{}.db",
        tmpdir.display(),
        uuid::Uuid::new_v4()
    );
    let _ = std::fs::remove_file(&path);

    let clock: Arc<dyn Clock> = Arc::new(TestClock);
    Database::open(&path, clock)
}

fn create_fixtures(db: &mut Database) -> Result<()> {
    let tx = db.transaction()?;
    let now = Utc::now().to_rfc3339();

    // Create capture
    tx.execute(
        "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, \
         locale, calendar, item_scope, route_id, entry_locked, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            CAPTURE_ID, TEXT, "2025-01-15T10:00:00Z", "America/New_York", -300,
            "en-US", "gregorian", "personal", "route-1", 0, now
        ],
    )?;

    // Create item
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
         sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            ITEM_ID,
            CAPTURE_ID,
            0,
            "active",
            "saved",
            "synced",
            "unprocessed",
            "completed",
            now,
            now
        ],
    )?;

    // Create job
    tx.execute(
        "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
         status, attempt_count, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![JOB_ID, 1, ITEM_ID, "interpret", 0, "running", 1, now],
    )?;

    // Create proposal row
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, \
         text_basis_kind, applied_state, abstained, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![PROPOSAL_ID, ITEM_ID, CAPTURE_ID, 0, 1, "original", "unapplied", 0, now],
    )?;

    tx.commit()?;
    Ok(())
}

#[test]
fn test_proposal_application_succeeds() -> Result<()> {
    let mut db = setup_database()?;
    create_fixtures(&mut db)?;

    let tx = db.transaction()?;

    let proposal = Proposal::new(
        PROPOSAL_ID.to_string(),
        ITEM_ID.to_string(),
        CAPTURE_ID.to_string(),
        0,
        SUPPORTED_PROPOSAL_SCHEMA_VERSION,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440005".to_string(),
    );

    // Change to have item_type with source evidence
    let mut proposal = proposal;
    proposal.item_type = Some(ItemType::Action);
    proposal.source_spans = Some(vec![SourceSpan::new(0, 4)]);

    let _ = apply_proposal(&tx, ITEM_ID, &proposal, JOB_ID, 1)?;

    // Verify item type was updated
    let item_type: Option<String> = tx.query_row(
        "SELECT item_type FROM items WHERE item_id = ?",
        [ITEM_ID],
        |row| row.get(0),
    )?;

    assert_eq!(item_type, Some("action".to_string()));

    // Verify job was completed
    let job_status: String = tx.query_row(
        "SELECT status FROM jobs WHERE job_id = ?",
        [JOB_ID],
        |row| row.get(0),
    )?;

    assert_eq!(job_status, "completed");

    Ok(())
}

#[test]
fn test_reminder_application_with_instant() -> Result<()> {
    let mut db = setup_database()?;
    create_fixtures(&mut db)?;

    let tx = db.transaction()?;

    let mut proposal = Proposal::new(
        PROPOSAL_ID.to_string(),
        ITEM_ID.to_string(),
        CAPTURE_ID.to_string(),
        0,
        SUPPORTED_PROPOSAL_SCHEMA_VERSION,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440005".to_string(),
    );

    proposal.item_type = Some(ItemType::Action);
    proposal.source_spans = Some(vec![SourceSpan::new(0, 4)]);
    proposal.reminder_proposal = Some(ReminderProposal {
        instant: Some("2025-01-16T09:00:00Z".to_string()),
        timezone_id: Some("America/New_York".to_string()),
        quality: TimeResolutionQuality::Explicit,
        source_span: Some(SourceSpan::new(23, 27)),
    });

    let _ = apply_proposal(&tx, ITEM_ID, &proposal, JOB_ID, 1)?;

    // Verify reminder has all required columns
    let (reminder_id, resolved_instant, timezone_id): (String, Option<String>, Option<String>) = tx
        .query_row(
            "SELECT reminder_id, resolved_instant, timezone_id FROM reminders WHERE item_id = ?",
            [ITEM_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

    assert!(!reminder_id.is_empty());
    assert_eq!(resolved_instant, Some("2025-01-16T09:00:00Z".to_string()));
    assert_eq!(timezone_id, Some("America/New_York".to_string()));

    Ok(())
}
