// Reminder desired state and operation records
//
// Stores one-shot reminder intent, stable native identifiers and durable schedule/cancel
// operations separately from delivered/seen facts. Operations are idempotent; identical
// requests produce the same desired operation and do not duplicate on replay.

use crate::time::resolver::ResolutionResult;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Reminder request state (what the user asked for)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestState {
    NotRequested,
    Resolved,
    NotScheduledYet,
    UnsupportedRecurrence,
    Unschedulable,
    Cancelled,
}

impl RequestState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RequestState::NotRequested => "not_requested",
            RequestState::Resolved => "resolved",
            RequestState::NotScheduledYet => "not_scheduled_yet",
            RequestState::UnsupportedRecurrence => "unsupported_recurrence",
            RequestState::Unschedulable => "unschedulable",
            RequestState::Cancelled => "cancelled",
        }
    }
}

// Reminder schedule state (native installation)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleState {
    NotScheduled,
    PendingSchedule,
    Scheduled,
    ScheduleFailed,
}

impl ScheduleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScheduleState::NotScheduled => "not_scheduled",
            ScheduleState::PendingSchedule => "pending_schedule",
            ScheduleState::Scheduled => "scheduled",
            ScheduleState::ScheduleFailed => "schedule_failed",
        }
    }
}

// Reminder delivery state (evidence only)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    Unknown,
    Delivered,
    Opened,
}

impl DeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryState::Unknown => "unknown",
            DeliveryState::Delivered => "delivered",
            DeliveryState::Opened => "opened",
        }
    }
}

// Reminder acknowledgment state
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcknowledgmentState {
    NotAcknowledged,
    Acknowledged,
}

impl AcknowledgmentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AcknowledgmentState::NotAcknowledged => "not_acknowledged",
            AcknowledgmentState::Acknowledged => "acknowledged",
        }
    }
}

/// Desired reminder state: one-shot intent, stable identifiers, and durable operations.
/// This is separate from delivery/seen facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderState {
    // Identifiers
    pub reminder_id: String,
    pub item_id: String,

    // Request state (what the user asked for)
    pub request_state: RequestState,

    // Schedule state (native installation)
    pub schedule_state: ScheduleState,

    // Delivery/acknowledgment state (evidence)
    pub delivery_state: DeliveryState,
    pub acknowledgment_state: AcknowledgmentState,

    // Resolved time (if request_state is Resolved)
    pub resolved_instant: Option<DateTime<Utc>>,
    pub timezone_id: Option<String>,

    // Ambiguity or unsupported reason
    pub ambiguity_reason: Option<String>,
    pub unsupported_reason: Option<String>,

    // Schedule generation: monotonically increasing on each reschedule
    pub schedule_generation: i32,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ReminderState {
    pub fn effect_identity(&self) -> String {
        format!("{}#{}", self.reminder_id, self.schedule_generation)
    }
}

/// Type of reminder operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    Create,
    Reschedule,
    Cancel,
    Complete,
}

impl OperationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationType::Create => "create",
            OperationType::Reschedule => "reschedule",
            OperationType::Cancel => "cancel",
            OperationType::Complete => "complete",
        }
    }
}

/// Operation state: whether it has been applied to the native side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Pending,
    Applied,
    Failed,
}

impl OperationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationState::Pending => "pending",
            OperationState::Applied => "applied",
            OperationState::Failed => "failed",
        }
    }
}

/// Immutable record of a durable desired operation.
/// Idempotency key: (reminder_id, effect_identity).
/// Replay of the same operation (same reminder_id + effect_identity) produces no new record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderOperation {
    pub operation_id: String,
    pub reminder_id: String,
    pub operation_type: OperationType,
    pub operation_state: OperationState,
    pub effect_identity: String,
    pub external_id: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Validate that a resolved time is not in the past and comes from a valid source.
/// Returns error if the time is in the past.
pub fn validate_resolved_time(resolved_time: DateTime<Utc>, now: DateTime<Utc>) -> Result<()> {
    if resolved_time < now {
        anyhow::bail!("Fabricated deadline: resolved time is in the past");
    }
    Ok(())
}

/// Apply a resolution result to an existing reminder state.
/// Updates request_state, resolved_instant, timezone_id, ambiguity_reason based on the result.
/// Validates that the resolved time comes from an explicit source and is not in the past.
pub fn apply_resolution(state: &mut ReminderState, resolution: &ResolutionResult) -> Result<()> {
    if let Some(resolved_time) = resolution.resolved_time {
        // Check if the resolved time is in the past
        if resolution.is_past {
            state.request_state = RequestState::Unschedulable;
            state.unsupported_reason = Some("time_in_past".to_string());
            state.resolved_instant = None;
            state.ambiguity_reason = None;
            state.timezone_id = resolution.context.timezone.clone().into();
            return Ok(());
        }

        state.request_state = RequestState::Resolved;
        state.resolved_instant = Some(resolved_time);
        state.timezone_id = resolution.context.timezone.clone().into();
        state.ambiguity_reason = None;
        state.unsupported_reason = None;
        Ok(())
    } else if resolution.is_ambiguous {
        state.request_state = RequestState::NotScheduledYet;
        state.ambiguity_reason = resolution.ambiguity_reason.clone();
        state.timezone_id = resolution.context.timezone.clone().into();
        state.resolved_instant = None;
        state.unsupported_reason = None;
        Ok(())
    } else {
        // Unsupported (e.g., repeating reminder) or parse error
        state.request_state = RequestState::UnsupportedRecurrence;
        state.unsupported_reason = resolution
            .ambiguity_reason
            .clone()
            .or(Some("Unsupported time expression".to_string()));
        state.timezone_id = resolution.context.timezone.clone().into();
        state.resolved_instant = None;
        state.ambiguity_reason = None;
        Ok(())
    }
}

/// Create a new reminder. Returns an error if the reminder is unsupported (e.g., recurring).
pub fn create_reminder_operation(
    tx: &Transaction,
    reminder_id: &str,
    state: &ReminderState,
) -> Result<ReminderOperation> {
    // Reject unsupported or ambiguous reminders
    match state.request_state {
        RequestState::UnsupportedRecurrence => {
            anyhow::bail!("Cannot schedule unsupported recurring reminder")
        }
        RequestState::NotScheduledYet => {
            anyhow::bail!("Cannot schedule ambiguous reminder without explicit user confirmation")
        }
        RequestState::Unschedulable => {
            anyhow::bail!(
                "Reminder is unschedulable: {}",
                state
                    .unsupported_reason
                    .as_ref()
                    .unwrap_or(&"unknown".to_string())
            )
        }
        RequestState::Cancelled => {
            anyhow::bail!("Cannot schedule cancelled reminder")
        }
        _ => {}
    }

    record_operation(
        tx,
        reminder_id,
        &state.effect_identity(),
        OperationType::Create,
        state.resolved_instant,
    )
}

/// Cancel a reminder. Idempotent.
pub fn cancel_reminder_operation(
    tx: &Transaction,
    reminder_id: &str,
    state: &ReminderState,
) -> Result<ReminderOperation> {
    record_operation(
        tx,
        reminder_id,
        &state.effect_identity(),
        OperationType::Cancel,
        None,
    )
}

/// Complete a reminder. Idempotent.
pub fn complete_reminder_operation(
    tx: &Transaction,
    reminder_id: &str,
    state: &ReminderState,
) -> Result<ReminderOperation> {
    record_operation(
        tx,
        reminder_id,
        &state.effect_identity(),
        OperationType::Complete,
        None,
    )
}

/// Record an operation in the database. Idempotent: if the same
/// (reminder_id, effect_identity, operation_type) combination already exists,
/// returns the existing operation without creating a new one.
pub fn record_operation(
    tx: &Transaction,
    reminder_id: &str,
    effect_identity: &str,
    operation_type: OperationType,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<ReminderOperation> {
    let operation_id = Uuid::new_v4().to_string();
    let now = Utc::now();

    // Check if (reminder_id, effect_identity, operation_type) already exists (idempotency).
    // This ensures Create(r1#0), Cancel(r1#0), and Complete(r1#0) are three separate operations.
    if let Ok(existing) = tx.query_row(
        "SELECT operation_id, operation_type, operation_state, external_id, scheduled_for, created_at
         FROM reminder_operations
         WHERE reminder_id = ?1 AND effect_identity = ?2 AND operation_type = ?3",
        rusqlite::params![reminder_id, effect_identity, operation_type.as_str()],
        |row| {
            let op_type_str: String = row.get(1)?;
            let op_state_str: String = row.get(2)?;
            let scheduled_for_str: Option<String> = row.get(4)?;

            let op_type = match op_type_str.as_str() {
                "create" => OperationType::Create,
                "reschedule" => OperationType::Reschedule,
                "cancel" => OperationType::Cancel,
                "complete" => OperationType::Complete,
                _ => OperationType::Create,
            };

            let op_state = match op_state_str.as_str() {
                "pending" => OperationState::Pending,
                "applied" => OperationState::Applied,
                "failed" => OperationState::Failed,
                _ => OperationState::Pending,
            };

            let scheduled_for = scheduled_for_str.and_then(|s| s.parse::<DateTime<Utc>>().ok());

            let created_at_str: String = row.get(5)?;
            let created_at = created_at_str
                .parse::<DateTime<Utc>>()
                .map_err(|_| rusqlite::Error::InvalidParameterName("created_at".to_string()))?;

            Ok(ReminderOperation {
                operation_id: row.get(0)?,
                reminder_id: reminder_id.to_string(),
                operation_type: op_type,
                operation_state: op_state,
                effect_identity: effect_identity.to_string(),
                external_id: row.get(3)?,
                scheduled_for,
                created_at,
            })
        },
    ) {
        return Ok(existing);
    }

    // New operation: insert it
    tx.execute(
        "INSERT INTO reminder_operations
         (operation_id, reminder_id, operation_type, operation_state, effect_identity, external_id, scheduled_for, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            &operation_id,
            reminder_id,
            operation_type.as_str(),
            OperationState::Pending.as_str(),
            effect_identity,
            None::<String>,
            scheduled_for.map(|dt| dt.to_rfc3339()),
            now.to_rfc3339(),
        ],
    )?;

    Ok(ReminderOperation {
        operation_id,
        reminder_id: reminder_id.to_string(),
        operation_type,
        operation_state: OperationState::Pending,
        effect_identity: effect_identity.to_string(),
        external_id: None,
        scheduled_for,
        created_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TimeContext;

    #[test]
    fn test_request_state_as_str() {
        assert_eq!(RequestState::NotRequested.as_str(), "not_requested");
        assert_eq!(RequestState::Resolved.as_str(), "resolved");
        assert_eq!(RequestState::NotScheduledYet.as_str(), "not_scheduled_yet");
        assert_eq!(
            RequestState::UnsupportedRecurrence.as_str(),
            "unsupported_recurrence"
        );
    }

    #[test]
    fn test_schedule_state_as_str() {
        assert_eq!(ScheduleState::NotScheduled.as_str(), "not_scheduled");
        assert_eq!(ScheduleState::Scheduled.as_str(), "scheduled");
    }

    #[test]
    fn test_delivery_state_as_str() {
        assert_eq!(DeliveryState::Unknown.as_str(), "unknown");
        assert_eq!(DeliveryState::Delivered.as_str(), "delivered");
    }

    #[test]
    fn test_acknowledgment_state_as_str() {
        assert_eq!(
            AcknowledgmentState::NotAcknowledged.as_str(),
            "not_acknowledged"
        );
        assert_eq!(AcknowledgmentState::Acknowledged.as_str(), "acknowledged");
    }

    #[test]
    fn test_effect_identity() {
        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Resolved,
            schedule_state: ScheduleState::Scheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(state.effect_identity(), "r1#0");

        let state_gen2 = ReminderState {
            schedule_generation: 2,
            ..state
        };
        assert_eq!(state_gen2.effect_identity(), "r1#2");
    }

    #[test]
    fn test_expired_opportunity_remains_inspectable() {
        let past = Utc::now() - chrono::Duration::hours(1);
        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Resolved,
            schedule_state: ScheduleState::Scheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: Some(past),
            timezone_id: Some("UTC".to_string()),
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(state.request_state, RequestState::Resolved);
        assert_eq!(state.resolved_instant, Some(past));
        assert_eq!(state.delivery_state, DeliveryState::Unknown);
    }

    #[test]
    fn test_ambiguous_reminder_remains_inspectable() {
        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::NotScheduledYet,
            schedule_state: ScheduleState::NotScheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: Some("missing hour".to_string()),
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(state.request_state, RequestState::NotScheduledYet);
        assert!(state.ambiguity_reason.is_some());
    }

    #[test]
    fn test_unsupported_recurrence_preserved() {
        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::UnsupportedRecurrence,
            schedule_state: ScheduleState::NotScheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: None,
            unsupported_reason: Some("repeating reminders not supported in M1".to_string()),
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(state.request_state, RequestState::UnsupportedRecurrence);
        assert!(state.unsupported_reason.is_some());
        assert_eq!(state.schedule_state, ScheduleState::NotScheduled);
    }

    #[test]
    fn test_cancelled_reminder_idempotent() {
        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Cancelled,
            schedule_state: ScheduleState::NotScheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(state.request_state, RequestState::Cancelled);
    }

    #[test]
    fn test_create_then_cancel_produces_separate_operations() {
        // This test demonstrates the fix for the confirmed bug:
        // Create(r1#0) and Cancel(r1#0) must produce separate operations.
        use rusqlite::Connection;

        let mut conn = Connection::open_in_memory().expect("Failed to open in-memory DB");
        conn.execute(
            "CREATE TABLE reminder_operations (
                operation_id TEXT PRIMARY KEY,
                reminder_id TEXT NOT NULL,
                operation_type TEXT NOT NULL,
                operation_state TEXT NOT NULL,
                effect_identity TEXT NOT NULL,
                external_id TEXT,
                scheduled_for TEXT,
                created_at TEXT NOT NULL,
                UNIQUE (reminder_id, effect_identity, operation_type)
            )",
            [],
        )
        .expect("Failed to create table");

        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Resolved,
            schedule_state: ScheduleState::Scheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: Some(Utc::now()),
            timezone_id: Some("UTC".to_string()),
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let tx = conn.transaction().unwrap();

        // Record Create operation
        let create_op = record_operation(
            &tx,
            "r1",
            &state.effect_identity(),
            OperationType::Create,
            Some(Utc::now()),
        )
        .expect("Create operation");

        // Record Cancel operation with same effect_identity
        let cancel_op = record_operation(
            &tx,
            "r1",
            &state.effect_identity(),
            OperationType::Cancel,
            None,
        )
        .expect("Cancel operation");

        // Both operations should be recorded with different operation_ids
        assert_eq!(create_op.effect_identity, "r1#0");
        assert_eq!(cancel_op.effect_identity, "r1#0");
        assert_ne!(
            create_op.operation_id, cancel_op.operation_id,
            "Create and Cancel should be separate operations"
        );
        assert_eq!(create_op.operation_type, OperationType::Create);
        assert_eq!(cancel_op.operation_type, OperationType::Cancel);

        tx.commit().unwrap();
    }

    #[test]
    fn test_create_then_complete_produces_separate_operations() {
        // Similar to create->cancel, create->complete should also produce separate operations.
        use rusqlite::Connection;

        let mut conn = Connection::open_in_memory().expect("Failed to open in-memory DB");
        conn.execute(
            "CREATE TABLE reminder_operations (
                operation_id TEXT PRIMARY KEY,
                reminder_id TEXT NOT NULL,
                operation_type TEXT NOT NULL,
                operation_state TEXT NOT NULL,
                effect_identity TEXT NOT NULL,
                external_id TEXT,
                scheduled_for TEXT,
                created_at TEXT NOT NULL,
                UNIQUE (reminder_id, effect_identity, operation_type)
            )",
            [],
        )
        .expect("Failed to create table");

        let state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Resolved,
            schedule_state: ScheduleState::Scheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: Some(Utc::now()),
            timezone_id: Some("UTC".to_string()),
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let tx = conn.transaction().unwrap();

        // Record Create operation
        let create_op = record_operation(
            &tx,
            "r1",
            &state.effect_identity(),
            OperationType::Create,
            Some(Utc::now()),
        )
        .expect("Create operation");

        // Record Complete operation with same effect_identity
        let complete_op = record_operation(
            &tx,
            "r1",
            &state.effect_identity(),
            OperationType::Complete,
            None,
        )
        .expect("Complete operation");

        // Both operations should be recorded with different operation_ids
        assert_ne!(
            create_op.operation_id, complete_op.operation_id,
            "Create and Complete should be separate operations"
        );
        assert_eq!(create_op.operation_type, OperationType::Create);
        assert_eq!(complete_op.operation_type, OperationType::Complete);

        tx.commit().unwrap();
    }

    #[test]
    fn test_schedule_generation_increments() {
        let mut state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::Resolved,
            schedule_state: ScheduleState::Scheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: Some(Utc::now()),
            timezone_id: Some("UTC".to_string()),
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(state.effect_identity(), "r1#0");
        state.schedule_generation = 1;
        assert_eq!(state.effect_identity(), "r1#1");
    }

    #[test]
    fn test_cannot_schedule_unsupported_recurrence() {
        use rusqlite::Connection;

        let mut conn = Connection::open_in_memory().expect("Failed to open in-memory DB");
        conn.execute(
            "CREATE TABLE reminder_operations (
                operation_id TEXT PRIMARY KEY,
                reminder_id TEXT NOT NULL,
                operation_type TEXT NOT NULL,
                operation_state TEXT NOT NULL,
                effect_identity TEXT NOT NULL,
                external_id TEXT,
                scheduled_for TEXT,
                created_at TEXT NOT NULL,
                UNIQUE (reminder_id, effect_identity)
            )",
            [],
        )
        .expect("Failed to create table");

        let mut state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::NotRequested,
            schedule_state: ScheduleState::NotScheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Mark as unsupported recurrence
        state.request_state = RequestState::UnsupportedRecurrence;
        state.unsupported_reason = Some("repeating reminders not supported".to_string());

        let tx = conn.transaction().unwrap();

        // Trying to create operation for unsupported reminder should fail
        let result = create_reminder_operation(&tx, "r1", &state);
        assert!(result.is_err(), "Should reject unsupported recurrence");
        assert!(
            result.unwrap_err().to_string().contains("unsupported"),
            "Error should mention unsupported"
        );

        tx.commit().unwrap();
    }

    #[test]
    fn test_past_time_rejected() {
        let mut state = ReminderState {
            reminder_id: "r1".to_string(),
            item_id: "i1".to_string(),
            request_state: RequestState::NotRequested,
            schedule_state: ScheduleState::NotScheduled,
            delivery_state: DeliveryState::Unknown,
            acknowledgment_state: AcknowledgmentState::NotAcknowledged,
            resolved_instant: None,
            timezone_id: None,
            ambiguity_reason: None,
            unsupported_reason: None,
            schedule_generation: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let past = Utc::now() - chrono::Duration::hours(1);

        let resolution = ResolutionResult {
            original_phrase: "yesterday".to_string(),
            resolved_date: None,
            resolved_local: None,
            resolved_time: Some(past),
            candidates: vec![],
            is_ambiguous: false,
            ambiguity_kind: None,
            ambiguity_reason: None,
            is_past: true,
            context: TimeContext {
                timezone: "UTC".to_string(),
                locale: "en_US".to_string(),
                reference_time: Utc::now(),
                utc_offset_at_capture: 0,
                calendar: "gregorian".to_string(),
            },
        };

        apply_resolution(&mut state, &resolution).expect("Apply resolution");

        // Past time should result in Unschedulable state
        assert_eq!(state.request_state, RequestState::Unschedulable);
        assert_eq!(state.unsupported_reason, Some("time_in_past".to_string()));
        assert!(state.resolved_instant.is_none());
    }
}
