use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

use super::record::{parse_instant, parse_notification_id};
use super::HistoryError;
use crate::reminders::state::{
    get_reminder, list_operations, list_request_events, AcknowledgmentState, DeliveryState,
    OperationState, OperationType, ReminderOperation, ReminderRecord, RequestEvent, RequestState,
    ScheduleState,
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

    /// The generation a native-side entry names; request entries without one are placed by the
    /// generation current when they were recorded.
    fn generation(&self) -> Option<i64> {
        match self {
            HistoryEntryKind::TimeRequested { generation, .. }
            | HistoryEntryKind::Delivered { generation }
            | HistoryEntryKind::Opened { generation }
            | HistoryEntryKind::Acknowledged { generation } => Some(*generation),
            HistoryEntryKind::Cancelled { generation, .. } => *generation,
            _ => None,
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
    /// The due time has passed, the OS supplied no delivery evidence and the user has not
    /// acknowledged it: the honest report is "elapsed, delivery unknown, not acknowledged",
    /// never missed, fired or noticed.
    pub fn elapsed_without_delivery_evidence(&self) -> bool {
        self.due == DueStatus::Elapsed
            && self.delivery == DeliveryState::Unknown
            && self.acknowledgment == AcknowledgmentState::NotAcknowledged
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

/// The inspectable history of one item's reminder, or `None` when the item has none or has been
/// deleted (tombstones are excluded from every result). Read-only.
pub fn reminder_history(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    item_id: &str,
) -> Result<Option<ReminderHistory>, HistoryError> {
    let lifecycle_state: Option<String> = tx
        .query_row(
            "SELECT lifecycle_state FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;
    if lifecycle_state.as_deref() == Some("deleted") {
        return Ok(None);
    }
    let Some(record) = get_reminder(tx, item_id)? else {
        return Ok(None);
    };
    let operations = list_operations(tx, &record.reminder_id)?;
    let request_events = list_request_events(tx, &record.reminder_id)?;
    // Each entry is paired with the generation it belongs to, so entries recorded at the same
    // instant still read in the order the requests were made.
    let mut placed: Vec<(i64, HistoryEntry)> = operation_entries(&operations)?
        .into_iter()
        .chain(fact_entries(tx, &record.reminder_id)?)
        .map(|entry| (entry.kind.generation().unwrap_or(0), entry))
        .collect();
    placed.extend(request_entries(&record, &request_events, &operations));
    placed.sort_by_key(|(generation, entry)| (entry.occurred_at, *generation, entry.kind.rank()));
    let entries = placed.into_iter().map(|(_, entry)| entry).collect();

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

/// Entries for every recorded request that no operation represents, oldest first. A request
/// can also reach such a state without a user request (capacity demotion, or expiry while held
/// back); when the current state is not the latest recorded request, it is added as its own
/// entry stamped with the reminder's last change.
fn request_entries(
    record: &ReminderRecord,
    request_events: &[RequestEvent],
    operations: &[ReminderOperation],
) -> Vec<(i64, HistoryEntry)> {
    let mut entries: Vec<(i64, HistoryEntry)> = request_events
        .iter()
        .filter_map(|event| {
            request_kind(
                event.request_state,
                event.resolved_instant,
                event.unsupported_reason.as_ref(),
                event.native_cancel,
            )
            .map(|kind| {
                let entry = HistoryEntry {
                    occurred_at: event.recorded_at,
                    kind,
                };
                (event.schedule_generation, entry)
            })
        })
        .collect();

    // A cancellation records no instant while the reminders row keeps its last one, so a
    // recorded cancellation of the current generation describes a cancelled row whatever its
    // retained instant.
    let current_is_recorded = request_events.last().is_some_and(|event| {
        event.request_state == record.request_state
            && event.schedule_generation == record.schedule_generation
            && (event.request_state == RequestState::Cancelled
                || event.resolved_instant == record.resolved_instant)
    });
    if !current_is_recorded {
        let native_cancel = operations.iter().any(|operation| {
            operation.operation_type == OperationType::Cancel
                && parse_notification_id(&operation.notification_id)
                    .is_some_and(|(_, generation)| generation == record.schedule_generation)
        });
        if let Some(kind) = request_kind(
            record.request_state,
            record.resolved_instant,
            record.unsupported_reason.as_ref(),
            native_cancel,
        ) {
            let entry = HistoryEntry {
                occurred_at: record.updated_at,
                kind,
            };
            entries.push((record.schedule_generation, entry));
        }
    }
    entries
}

/// The entry for a request that installs nothing. A cancellation that retired a native request
/// has none of its own: the cancel operation's entry describes it.
fn request_kind(
    request_state: RequestState,
    resolved_instant: Option<DateTime<Utc>>,
    unsupported_reason: Option<&String>,
    native_cancel: bool,
) -> Option<HistoryEntryKind> {
    match request_state {
        RequestState::UnsupportedRecurrence => Some(HistoryEntryKind::RepeatRequested {
            reason: unsupported_reason.cloned(),
        }),
        RequestState::NotScheduledYet | RequestState::Unschedulable(_) => {
            Some(HistoryEntryKind::NotSchedulable {
                request_state,
                due_at: resolved_instant,
            })
        }
        RequestState::Cancelled if !native_cancel => Some(HistoryEntryKind::Cancelled {
            generation: None,
            native_cancel: None,
        }),
        _ => None,
    }
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
