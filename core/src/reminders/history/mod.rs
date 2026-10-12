// Reminder history (N06): what was requested, what core installed or cancelled, and what the OS
// or the user actually reported, as inspectable facts.
//
// Evidence rules:
// - History is assembled only from recorded facts. Requested times and installation or
//   cancellation outcomes come from the reminder's durable operations; delivery and opening come
//   from OS events (`record_delivery_evidence`); acknowledgment comes from an explicit user action
//   (`acknowledge_reminder`). The clock is read for exactly one thing: whether the due time lies
//   in the past (`DueStatus`). It never produces a delivered, opened, missed, seen or
//   acknowledged fact, and never completes the item.
// - An elapsed reminder with no OS evidence is `DueStatus::Elapsed` with `DeliveryState::Unknown`
//   and `AcknowledgmentState::NotAcknowledged` (unacknowledged). The history has no entry for it;
//   absence of evidence is reported as unknown, not as a missed or fired reminder.
// - Delivery and acknowledgment are separate facts. Acknowledging changes neither delivery nor
//   the item lifecycle, and delivery or opening never acknowledges. A repeat request is its own
//   entry (`RepeatRequested`): it is not an acknowledgment and, because recurring reminders are
//   unsupported in M1, installs nothing.
// - Facts are keyed by (reminder, schedule generation, kind) and the first observation wins, so
//   duplicate or late callbacks change nothing. A fact about an older generation stays in the
//   history but never touches the current generation's delivery or acknowledgment state; a new
//   generation starts with both unknown/unacknowledged (see `reminders::state`).
// - History is read per item on request. There is no cross-item feed, digest or backlog count,
//   so returning after a lapse replays nothing; events that arrive late are stored as evidence
//   and surface only when the item's history is opened.
// - Timestamps are honest about what they are: a request or cancellation entry is stamped when
//   core recorded it, an OS or user entry when its source says it happened. Installation is
//   reported as the current status of the request entry, never as a made-up time.

mod record;
mod view;

use thiserror::Error;

use crate::reminders::state::ReminderStateError;

pub use record::{
    acknowledge_reminder, record_delivery_evidence, DeliveryEvidence, DeliveryEvidenceKind,
    RecordOutcome, UserAcknowledgment,
};
pub use view::{
    reminder_history, DueStatus, HistoryEntry, HistoryEntryKind, NativeEffectStatus,
    ReminderHistory, ReminderSummary,
};

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("notification identifier {0} does not name a schedule core requested")]
    UnknownNotification(String),
    #[error("notification {notification_id} belongs to another item than {item_id}")]
    ItemMismatch {
        notification_id: String,
        item_id: String,
    },
    #[error("item {0} was deleted; no further reminder facts are recorded")]
    ItemDeleted(String),
    #[error("observation time {0} lies in the future")]
    FutureObservation(String),
    #[error(transparent)]
    State(#[from] ReminderStateError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error("stored reminder history is invalid: {0}")]
    Corrupt(String),
}
