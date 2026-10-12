use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, Transaction};

use super::HistoryError;
use crate::reminders::state::notification_identifier;
use crate::store::schema::Clock;

/// How far ahead of core's clock a reported observation may lie before it is refused as
/// implausible. Absorbs ordinary clock skew between the OS event and core's clock.
const MAX_CLOCK_SKEW_SECONDS: i64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryEvidenceKind {
    Delivered,
    Opened,
}

impl DeliveryEvidenceKind {
    fn column_value(self) -> &'static str {
        match self {
            DeliveryEvidenceKind::Delivered => "delivered",
            DeliveryEvidenceKind::Opened => "opened",
        }
    }
}

/// A delivery or opening event the OS reported for one notification request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryEvidence {
    /// The native request identifier, `reminder_id#generation`, exactly as core scheduled it.
    pub notification_id: String,
    pub kind: DeliveryEvidenceKind,
    /// When the OS says it happened.
    pub observed_at: DateTime<Utc>,
}

/// The user explicitly acknowledging the notification they saw. Not a completion of the item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserAcknowledgment {
    pub item_id: String,
    pub notification_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    /// The fact is now stored. `current_generation` says whether it also set the reminder's
    /// delivery or acknowledgment state (it did not for an older generation).
    Recorded { current_generation: bool },
    /// The first observation of this fact stands; nothing was changed.
    AlreadyRecorded,
}

/// Store OS evidence that a notification was delivered or opened.
pub fn record_delivery_evidence(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    evidence: &DeliveryEvidence,
) -> Result<RecordOutcome, HistoryError> {
    record_fact(
        tx,
        clock,
        None,
        &evidence.notification_id,
        evidence.kind.column_value(),
        evidence.observed_at,
    )
}

/// Store the user's explicit acknowledgment, stamped with core's clock at the moment of the
/// action. Retrying it, or acknowledging again, returns `AlreadyRecorded`.
pub fn acknowledge_reminder(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    acknowledgment: &UserAcknowledgment,
) -> Result<RecordOutcome, HistoryError> {
    record_fact(
        tx,
        clock,
        Some(&acknowledgment.item_id),
        &acknowledgment.notification_id,
        "acknowledged",
        clock.now(),
    )
}

/// Split `reminder_id#generation`.
pub(super) fn parse_notification_id(notification_id: &str) -> Option<(&str, i64)> {
    let (reminder_id, generation) = notification_id.rsplit_once('#')?;
    let generation: i64 = generation.parse().ok()?;
    (!reminder_id.is_empty() && generation > 0).then_some((reminder_id, generation))
}

fn record_fact(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    expected_item_id: Option<&str>,
    notification_id: &str,
    event_kind: &str,
    observed_at: DateTime<Utc>,
) -> Result<RecordOutcome, HistoryError> {
    let unknown = || HistoryError::UnknownNotification(notification_id.to_string());
    let (reminder_id, generation) = parse_notification_id(notification_id).ok_or_else(unknown)?;

    let row: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT reminders.item_id, reminders.schedule_generation, items.lifecycle_state
             FROM reminders JOIN items ON items.item_id = reminders.item_id
             WHERE reminders.reminder_id = ?",
            [reminder_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (item_id, current_generation, lifecycle_state) = row.ok_or_else(unknown)?;

    if let Some(expected) = expected_item_id {
        if expected != item_id {
            return Err(HistoryError::ItemMismatch {
                notification_id: notification_id.to_string(),
                item_id: expected.to_string(),
            });
        }
    }
    if lifecycle_state == "deleted" {
        return Err(HistoryError::ItemDeleted(item_id));
    }

    // Only a request core actually made can have been delivered, opened or acknowledged.
    let requested: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM reminder_operations
                       WHERE reminder_id = ? AND effect_identity = ?)",
        rusqlite::params![reminder_id, format!("schedule:{notification_id}")],
        |row| row.get(0),
    )?;
    if !requested || notification_identifier(reminder_id, generation) != notification_id {
        return Err(unknown());
    }

    let now = clock.now();
    if observed_at > now + Duration::seconds(MAX_CLOCK_SKEW_SECONDS) {
        return Err(HistoryError::FutureObservation(format_instant(observed_at)));
    }

    let inserted = tx.execute(
        "INSERT OR IGNORE INTO reminder_history_events
            (reminder_id, schedule_generation, event_kind, observed_at, recorded_at)
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            reminder_id,
            generation,
            event_kind,
            format_instant(observed_at),
            format_instant(now)
        ],
    )?;
    if inserted == 0 {
        return Ok(RecordOutcome::AlreadyRecorded);
    }

    let is_current = generation == current_generation;
    if is_current {
        apply_to_current_state(tx, reminder_id, event_kind)?;
    }
    Ok(RecordOutcome::Recorded {
        current_generation: is_current,
    })
}

/// Raise the reminder's delivery state, or set its acknowledgment, for the live generation.
/// Delivery only moves forward (`unknown`, `delivered`, `opened`), so a late `delivered` after
/// `opened` changes nothing. These are native-side facts and do not bump `state_version`,
/// which guards user intent.
fn apply_to_current_state(
    tx: &Transaction<'_>,
    reminder_id: &str,
    event_kind: &str,
) -> Result<(), HistoryError> {
    match event_kind {
        "acknowledged" => {
            tx.execute(
                "UPDATE reminders SET acknowledgment_state = ? WHERE reminder_id = ?",
                rusqlite::params!["acknowledged", reminder_id],
            )?;
        }
        "delivered" => {
            tx.execute(
                "UPDATE reminders SET delivery_state = ?
                 WHERE reminder_id = ? AND delivery_state = 'unknown'",
                rusqlite::params!["delivered", reminder_id],
            )?;
        }
        _ => {
            tx.execute(
                "UPDATE reminders SET delivery_state = ? WHERE reminder_id = ?",
                rusqlite::params!["opened", reminder_id],
            )?;
        }
    }
    Ok(())
}

pub(super) fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(super) fn parse_instant(text: &str) -> Result<DateTime<Utc>, HistoryError> {
    DateTime::parse_from_rfc3339(text)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| HistoryError::Corrupt(format!("timestamp '{text}'")))
}
