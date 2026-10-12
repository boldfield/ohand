use chrono::{DateTime, Utc};
use rusqlite::Transaction;

use super::record::{parse_instant, parse_notification_id};
use super::HistoryError;
use crate::reminders::state::{
    get_reminder, list_operations, AcknowledgmentState, DeliveryState, OperationState,
    OperationType, ReminderOperation, ReminderRecord, RequestState, ScheduleState,
};
use crate::store::schema::Clock;

/// Whether the due time lies in the past. This is the only thing the clock decides; it says
/// nothing about delivery, attention or acknowledgment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DueStatus {
    /// No single due time applies (not requested, ambiguous, recurring, unschedulable for a
    /// reason other than the past, or cancelled).
    NotApplicable,
    Upcoming,
    Elapsed,
}

/// What core's native request for one operation is known to have done, from its recorded
/// state. `AttemptedUnconfirmed` means the call was made and its outcome was not confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeEffectStatus {
    NotAttempted,
    AttemptedUnconfirmed,
    Rejected,
    Confirmed,
    Superseded,
}

impl From<OperationState> for NativeEffectStatus {
    fn from(state: OperationState) -> Self {
        match state {
            OperationState::Pending => NativeEffectStatus::NotAttempted,
            OperationState::Failed => NativeEffectStatus::AttemptedUnconfirmed,
            OperationState::Rejected => NativeEffectStatus::Rejected,
            OperationState::Acknowledged => NativeEffectStatus::Confirmed,
            OperationState::Superseded => NativeEffectStatus::Superseded,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryEntryKind {
    /// A one-shot time was requested for a schedule generation. `installation` is the current
    /// status of the native install, not an event with its own time.
    TimeRequested {
        generation: i64,
        due_at: DateTime<Utc>,
        installation: NativeEffectStatus,
    },
    /// A repeating reminder was requested. Unsupported: nothing was or will be scheduled, and
    /// it is not an acknowledgment of anything.
    RepeatRequested { reason: Option<String> },
    /// A time was requested but no notification could be requested for it (ambiguous time, or
    /// unschedulable for the stated reason).
    NotSchedulable {
        request_state: RequestState,
        due_at: Option<DateTime<Utc>>,
    },
    /// The reminder was cancelled. `generation` and `native_cancel` are present when a native
    /// request existed to cancel.
    Cancelled {
        generation: Option<i64>,
        native_cancel: Option<NativeEffectStatus>,
    },
    /// The OS reported the notification delivered.
    Delivered { generation: i64 },
    /// The OS reported the user opened the app through the notification.
    Opened { generation: i64 },
    /// The user explicitly acknowledged the reminder.
    Acknowledged { generation: i64 },
}

impl HistoryEntryKind {
    fn rank(&self) -> u8 {
        match self {
            HistoryEntryKind::TimeRequested { .. }
            | HistoryEntryKind::RepeatRequested { .. }
            | HistoryEntryKind::NotSchedulable { .. } => 0,
            HistoryEntryKind::Cancelled { .. } => 1,
            HistoryEntryKind::Delivered { .. } => 2,
            HistoryEntryKind::Opened { .. } => 3,
            HistoryEntryKind::Acknowledged { .. } => 4,
        }
    }

    fn generation(&self) -> i64 {
        match self {
            HistoryEntryKind::TimeRequested { generation, .. }
            | HistoryEntryKind::Delivered { generation }
            | HistoryEntryKind::Opened { generation }
            | HistoryEntryKind::Acknowledged { generation } => *generation,
            HistoryEntryKind::Cancelled { generation, .. } => generation.unwrap_or(0),
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// For core's own records, when core recorded them; for OS and user facts, when the source
    /// says they happened.
    pub occurred_at: DateTime<Utc>,
    pub kind: HistoryEntryKind,
}

/// The current standing of the live generation. `due` is computed from the clock; `delivery`
/// and `acknowledgment` are only ever what recorded facts set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSummary {
    pub request_state: RequestState,
    pub schedule_state: ScheduleState,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone_id: Option<String>,
    pub due: DueStatus,
    pub delivery: DeliveryState,
    pub acknowledgment: AcknowledgmentState,
}

impl ReminderSummary {
    /// The due time has passed and the OS supplied no delivery evidence: the honest report is
    /// "elapsed, delivery unknown, not acknowledged", never missed, fired or noticed.
    pub fn elapsed_without_delivery_evidence(&self) -> bool {
        self.due == DueStatus::Elapsed && self.delivery == DeliveryState::Unknown
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderHistory {
    pub reminder_id: String,
    pub item_id: String,
    pub summary: ReminderSummary,
    /// Oldest first.
    pub entries: Vec<HistoryEntry>,
}

/// The inspectable history of one item's reminder, or `None` when the item has none. Read-only.
pub fn reminder_history(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    item_id: &str,
) -> Result<Option<ReminderHistory>, HistoryError> {
    let Some(record) = get_reminder(tx, item_id)? else {
        return Ok(None);
    };
    let operations = list_operations(tx, &record.reminder_id)?;
    let mut entries = operation_entries(&operations)?;
    entries.extend(request_entries(&record, &operations));
    entries.extend(fact_entries(tx, &record.reminder_id)?);
    entries.sort_by_key(|entry| {
        (
            entry.occurred_at,
            entry.kind.rank(),
            entry.kind.generation(),
        )
    });

    Ok(Some(ReminderHistory {
        summary: summarize(&record, clock.now()),
        reminder_id: record.reminder_id,
        item_id: record.item_id,
        entries,
    }))
}

fn summarize(record: &ReminderRecord, now: DateTime<Utc>) -> ReminderSummary {
    let due = match record.request_state {
        RequestState::Resolved | RequestState::Unschedulable(_)
            if record.resolved_instant.is_some() =>
        {
            if record.due_time_passed(now) {
                DueStatus::Elapsed
            } else {
                DueStatus::Upcoming
            }
        }
        _ => DueStatus::NotApplicable,
    };
    ReminderSummary {
        request_state: record.request_state,
        schedule_state: record.schedule_state,
        due_at: record.resolved_instant,
        timezone_id: record.timezone_id.clone(),
        due,
        delivery: record.delivery_state,
        acknowledgment: record.acknowledgment_state,
    }
}

fn operation_entries(operations: &[ReminderOperation]) -> Result<Vec<HistoryEntry>, HistoryError> {
    operations
        .iter()
        .map(|operation| {
            let (_, generation) = parse_notification_id(&operation.notification_id)
                .ok_or_else(|| HistoryError::Corrupt(operation.notification_id.clone()))?;
            let status = NativeEffectStatus::from(operation.operation_state);
            let kind = match operation.operation_type {
                OperationType::Schedule => HistoryEntryKind::TimeRequested {
                    generation,
                    due_at: operation.scheduled_for.ok_or_else(|| {
                        HistoryError::Corrupt(format!("schedule without time {generation}"))
                    })?,
                    installation: status,
                },
                OperationType::Cancel => HistoryEntryKind::Cancelled {
                    generation: Some(generation),
                    native_cancel: Some(status),
                },
            };
            Ok(HistoryEntry {
                occurred_at: operation.created_at,
                kind,
            })
        })
        .collect()
}

/// Entries for the reminder's current request that no operation represents.
fn request_entries(record: &ReminderRecord, operations: &[ReminderOperation]) -> Vec<HistoryEntry> {
    let kind = match record.request_state {
        RequestState::UnsupportedRecurrence => HistoryEntryKind::RepeatRequested {
            reason: record.unsupported_reason.clone(),
        },
        RequestState::NotScheduledYet | RequestState::Unschedulable(_) => {
            HistoryEntryKind::NotSchedulable {
                request_state: record.request_state,
                due_at: record.resolved_instant,
            }
        }
        RequestState::Cancelled
            if !operations
                .iter()
                .any(|operation| operation.operation_type == OperationType::Cancel) =>
        {
            HistoryEntryKind::Cancelled {
                generation: None,
                native_cancel: None,
            }
        }
        _ => return Vec::new(),
    };
    vec![HistoryEntry {
        occurred_at: record.updated_at,
        kind,
    }]
}

fn fact_entries(
    tx: &Transaction<'_>,
    reminder_id: &str,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    let mut statement = tx.prepare(
        "SELECT schedule_generation, event_kind, observed_at
         FROM reminder_history_events WHERE reminder_id = ?",
    )?;
    let rows = statement
        .query_map([reminder_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(generation, event_kind, observed_at)| {
            let kind = match event_kind.as_str() {
                "delivered" => HistoryEntryKind::Delivered { generation },
                "opened" => HistoryEntryKind::Opened { generation },
                "acknowledged" => HistoryEntryKind::Acknowledged { generation },
                other => return Err(HistoryError::Corrupt(format!("event kind '{other}'"))),
            };
            Ok(HistoryEntry {
                occurred_at: parse_instant(&observed_at)?,
                kind,
            })
        })
        .collect()
}
