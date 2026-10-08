// Reminder desired state and operations
//
// Stores one-shot reminder intent, stable native identifiers and durable schedule/cancel
// operations separately from delivered/seen facts. Operations are idempotent; creating,
// editing, cancelling or completing an action produces a deterministic operation record.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Represents a user's one-shot reminder intent: what they want to be reminded about
/// and when. This is separate from whether a native reminder has been scheduled or
/// whether a notification has been delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderIntent {
    /// Unique identifier for this reminder intent (stable, from source capture).
    pub reminder_id: String,
    /// The original user utterance requesting the reminder.
    pub source_text: String,
    /// When the user requested this reminder for (resolution result).
    pub requested_time: RemindTime,
    /// Whether the requested time has ambiguity that requires user clarification.
    pub is_ambiguous: bool,
    /// If ambiguous, the reason (e.g., "missing hour", "DST fold").
    pub ambiguity_reason: Option<String>,
    /// Whether the requested time is in the past relative to capture reference time.
    pub is_past: bool,
    /// Metadata: when this intent was captured.
    pub captured_at: DateTime<Utc>,
}

/// The time a user requested for a reminder. May be fully resolved, ambiguous,
/// or unable to parse (unsupported format like repeating reminders).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemindTime {
    /// A specific time the user requested; fully resolved and unambiguous.
    Scheduled(DateTime<Utc>),
    /// A partial time (e.g., "Friday" with no hour). Requires clarification.
    Ambiguous {
        original_phrase: String,
        // If a partial date was extracted
        resolved_date: Option<chrono::NaiveDate>,
        // If a partial local time was extracted
        resolved_local: Option<chrono::NaiveDateTime>,
    },
    /// The phrase requested something unsupported (e.g., repeating reminders).
    /// The original phrase is preserved; this reminder stays unscheduled.
    Unsupported {
        original_phrase: String,
        reason: String,
    },
    /// The time couldn't be parsed at all (format error, invalid input, etc.).
    Unparseable {
        original_phrase: String,
        reason: String,
    },
}

impl RemindTime {
    pub fn is_scheduled(&self) -> bool {
        matches!(self, RemindTime::Scheduled(_))
    }

    pub fn is_ambiguous(&self) -> bool {
        matches!(self, RemindTime::Ambiguous { .. })
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self, RemindTime::Unsupported { .. })
    }

    pub fn is_unparseable(&self) -> bool {
        matches!(self, RemindTime::Unparseable { .. })
    }
}

impl fmt::Display for RemindTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemindTime::Scheduled(dt) => write!(f, "Scheduled: {}", dt.to_rfc3339()),
            RemindTime::Ambiguous {
                original_phrase, ..
            } => {
                write!(f, "Ambiguous: {}", original_phrase)
            }
            RemindTime::Unsupported {
                original_phrase,
                reason,
            } => write!(f, "Unsupported ({}): {}", reason, original_phrase),
            RemindTime::Unparseable {
                original_phrase,
                reason,
            } => write!(f, "Unparseable ({}): {}", reason, original_phrase),
        }
    }
}

/// A user operation on a reminder: schedule, reschedule, cancel, or complete.
/// Operations are immutable once recorded and designed to be idempotent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderOperation {
    /// Operation unique ID (generated at recording time).
    pub operation_id: String,
    /// The reminder this operation applies to.
    pub reminder_id: String,
    /// What the user did.
    pub operation_type: OperationType,
    /// When this operation was recorded.
    pub recorded_at: DateTime<Utc>,
    /// Stable native identifier set by this operation (e.g., iOS notification ID).
    /// Only set on Schedule operations; other operations reference an existing native_id.
    pub native_id: Option<String>,
}

/// The type of operation a user performed on a reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    /// User created the reminder with the given intent (maybe ambiguous, maybe scheduled).
    Created { intent: ReminderIntent },
    /// User rescheduled the reminder to a different time or clarified an ambiguity.
    Rescheduled { new_intent: ReminderIntent },
    /// User cancelled/dismissed the reminder before it triggers.
    Cancelled,
    /// User marked the reminder as complete (they did the action).
    Completed,
}

/// The current authoritative state of a reminder: its desired intent and operations.
/// This is the projection of all operations applied to a reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderState {
    /// The reminder's unique identifier.
    pub reminder_id: String,
    /// The current desired intent (from the most recent Create or Reschedule operation).
    pub current_intent: ReminderIntent,
    /// Lifecycle: whether the reminder is active, cancelled, or completed.
    pub lifecycle_state: ReminderLifecycle,
    /// All operations applied to this reminder, in order. Used to detect idempotency
    /// and rebuild state.
    pub operations: Vec<ReminderOperation>,
    /// The stable native identifier set when the reminder was scheduled (if any).
    /// None if the reminder is still ambiguous or past.
    pub native_id: Option<String>,
}

/// The lifecycle of a reminder: active (waiting for trigger), cancelled or completed (terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderLifecycle {
    /// Reminder is waiting (either for a future scheduled time or for clarification if ambiguous).
    Active,
    /// User cancelled the reminder; it will not trigger.
    Cancelled,
    /// Reminder has been completed/fulfilled (or the user marked it done).
    Completed,
}

impl ReminderLifecycle {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderLifecycle::Active => "active",
            ReminderLifecycle::Cancelled => "cancelled",
            ReminderLifecycle::Completed => "completed",
        }
    }
}

impl fmt::Display for ReminderLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Validates whether an operation can be applied to the current reminder state.
pub fn validate_operation(current: &ReminderState, op: &OperationType) -> OperationValidity {
    use OperationValidity::*;

    match current.lifecycle_state {
        ReminderLifecycle::Completed => {
            // Completed reminders only accept idempotent completion.
            match op {
                OperationType::Completed => Valid,
                _ => NotAllowed,
            }
        }
        ReminderLifecycle::Cancelled => {
            // Cancelled reminders only accept idempotent cancellation.
            match op {
                OperationType::Cancelled => Valid,
                _ => NotAllowed,
            }
        }
        ReminderLifecycle::Active => {
            // Active reminders can be rescheduled, cancelled, or completed.
            match op {
                OperationType::Created { .. } => NotAllowed, // Already created
                OperationType::Rescheduled { .. } => Valid,
                OperationType::Cancelled => Valid,
                OperationType::Completed => Valid,
            }
        }
    }
}

/// Outcome of validating an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationValidity {
    /// The operation is allowed.
    Valid,
    /// The operation is not allowed from this lifecycle state.
    NotAllowed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_intent(reminder_id: &str, requested_time: RemindTime) -> ReminderIntent {
        let is_ambiguous = matches!(requested_time, RemindTime::Ambiguous { .. });
        let ambiguity_reason = match &requested_time {
            RemindTime::Ambiguous { .. } => Some("missing hour".to_string()),
            _ => None,
        };
        ReminderIntent {
            reminder_id: reminder_id.to_string(),
            source_text: "remind me later".to_string(),
            requested_time,
            is_ambiguous,
            ambiguity_reason,
            is_past: false,
            captured_at: Utc::now(),
        }
    }

    fn make_state(reminder_id: &str, intent: ReminderIntent) -> ReminderState {
        ReminderState {
            reminder_id: reminder_id.to_string(),
            current_intent: intent,
            lifecycle_state: ReminderLifecycle::Active,
            operations: vec![],
            native_id: None,
        }
    }

    #[test]
    fn test_remind_time_scheduled_variant() {
        let now = Utc::now();
        let time = RemindTime::Scheduled(now);
        assert!(time.is_scheduled());
        assert!(!time.is_ambiguous());
        assert!(!time.is_unsupported());
    }

    #[test]
    fn test_remind_time_ambiguous_variant() {
        let time = RemindTime::Ambiguous {
            original_phrase: "Friday".to_string(),
            resolved_date: None,
            resolved_local: None,
        };
        assert!(!time.is_scheduled());
        assert!(time.is_ambiguous());
    }

    #[test]
    fn test_remind_time_unsupported_repeat() {
        let time = RemindTime::Unsupported {
            original_phrase: "remind me every day".to_string(),
            reason: "repeating reminders not supported in M1".to_string(),
        };
        assert!(time.is_unsupported());
        assert!(!time.is_scheduled());
    }

    #[test]
    fn test_remind_time_unparseable() {
        let time = RemindTime::Unparseable {
            original_phrase: "xyz@$!%".to_string(),
            reason: "invalid date format".to_string(),
        };
        assert!(time.is_unparseable());
    }

    #[test]
    fn test_operation_validity_schedule_then_reschedule() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent1 = make_intent("r1", RemindTime::Scheduled(future));
        let mut state = make_state("r1", intent1);
        state.operations.push(ReminderOperation {
            operation_id: "op1".to_string(),
            reminder_id: "r1".to_string(),
            operation_type: OperationType::Created {
                intent: state.current_intent.clone(),
            },
            recorded_at: Utc::now(),
            native_id: Some("native1".to_string()),
        });

        let future2 = Utc::now() + chrono::Duration::hours(2);
        let intent2 = make_intent("r1", RemindTime::Scheduled(future2));
        let op = OperationType::Rescheduled {
            new_intent: intent2,
        };
        assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);
    }

    #[test]
    fn test_operation_validity_active_to_cancelled() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent = make_intent("r1", RemindTime::Scheduled(future));
        let state = make_state("r1", intent);

        let op = OperationType::Cancelled;
        assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);
    }

    #[test]
    fn test_operation_validity_cancelled_idempotent() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent = make_intent("r1", RemindTime::Scheduled(future));
        let mut state = make_state("r1", intent);
        state.lifecycle_state = ReminderLifecycle::Cancelled;

        // Idempotent: can cancel an already-cancelled reminder
        let op = OperationType::Cancelled;
        assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);
    }

    #[test]
    fn test_operation_validity_cancelled_rejects_reschedule() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent = make_intent("r1", RemindTime::Scheduled(future));
        let mut state = make_state("r1", intent);
        state.lifecycle_state = ReminderLifecycle::Cancelled;

        let future2 = Utc::now() + chrono::Duration::hours(2);
        let intent2 = make_intent("r1", RemindTime::Scheduled(future2));
        let op = OperationType::Rescheduled {
            new_intent: intent2,
        };
        assert_eq!(
            validate_operation(&state, &op),
            OperationValidity::NotAllowed
        );
    }

    #[test]
    fn test_operation_validity_completed_idempotent() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent = make_intent("r1", RemindTime::Scheduled(future));
        let mut state = make_state("r1", intent);
        state.lifecycle_state = ReminderLifecycle::Completed;

        // Idempotent: can complete an already-completed reminder
        let op = OperationType::Completed;
        assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);
    }

    #[test]
    fn test_operation_validity_completed_rejects_cancel() {
        let future = Utc::now() + chrono::Duration::hours(1);
        let intent = make_intent("r1", RemindTime::Scheduled(future));
        let mut state = make_state("r1", intent);
        state.lifecycle_state = ReminderLifecycle::Completed;

        let op = OperationType::Cancelled;
        assert_eq!(
            validate_operation(&state, &op),
            OperationValidity::NotAllowed
        );
    }

    #[test]
    fn test_ambiguous_remainder_inspectable() {
        let time = RemindTime::Ambiguous {
            original_phrase: "Friday".to_string(),
            resolved_date: None,
            resolved_local: None,
        };
        let intent = make_intent("r1", time);
        let state = make_state("r1", intent);

        assert!(state.current_intent.is_ambiguous);
        assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
    }

    #[test]
    fn test_past_reminder_inspectable() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let mut intent = make_intent("r1", RemindTime::Scheduled(past));
        intent.is_past = true;
        let state = make_state("r1", intent);

        assert!(state.current_intent.is_past);
        assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
    }

    #[test]
    fn test_unsupported_repeat_preserved() {
        let time = RemindTime::Unsupported {
            original_phrase: "remind me every Monday".to_string(),
            reason: "repeating reminders not supported in M1".to_string(),
        };
        let intent = make_intent("r1", time);
        let state = make_state("r1", intent);

        assert!(state.current_intent.requested_time.is_unsupported());
        // Should remain inspectable; not silently converted to one-shot
        assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
        assert!(!state.current_intent.requested_time.is_scheduled());
    }

    #[test]
    fn test_lifecycle_state_display() {
        assert_eq!(ReminderLifecycle::Active.as_str(), "active");
        assert_eq!(ReminderLifecycle::Cancelled.as_str(), "cancelled");
        assert_eq!(ReminderLifecycle::Completed.as_str(), "completed");
    }
}
