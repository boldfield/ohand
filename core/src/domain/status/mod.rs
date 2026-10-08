// Save, sync, processing, transcription, and reminder states
//
// These types represent distinct independent facts about item status per F01 contract:
// - SaveState: durability of source on this device
// - SyncState: synchronization state (M1: always not_configured)
// - ProcessingState: interpretation/AI processing result
// - TranscriptionState: audio transcription result
// - ReminderRequestState: what the user asked for
// - ReminderScheduleState: native installation state
// - ReminderDeliveryState: evidence from OS
// - ReminderAcknowledgmentState: user's response to reminder
//
// Permission denial, provider outage, expired scheduling opportunity, and pending
// ambiguity each have explicit distinguishable status values and never claim
// user attention by themselves.

use crate::jobs::queue::JobStatus;
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug)]
pub struct ParseStateError(pub String);

impl fmt::Display for ParseStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseStateError {}

/// Local save state: durability of source on this device.
/// Per F01 contract: whether source is durably committed on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Source not yet durably committed on this device.
    NotSaved,
    /// Source durably committed on this device and acknowledged.
    SavedLocal,
}

impl SaveState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SaveState::NotSaved => "not_saved",
            SaveState::SavedLocal => "saved_local",
        }
    }
}

impl fmt::Display for SaveState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SaveState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_saved" => Ok(SaveState::NotSaved),
            "saved_local" => Ok(SaveState::SavedLocal),
            _ => Err(ParseStateError(format!("Unknown SaveState: {}", s))),
        }
    }
}

/// Sync state: whether the capture has been synchronized to another device or server.
/// M1 does not support sync, so all items are "not_configured".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Sync is not configured (M1 baseline).
    NotConfigured,
}

impl SyncState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncState::NotConfigured => "not_configured",
        }
    }
}

impl fmt::Display for SyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SyncState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_configured" => Ok(SyncState::NotConfigured),
            _ => Err(ParseStateError(format!("Unknown SyncState: {}", s))),
        }
    }
}

/// Processing state: interpretation/AI processing result per F01 contract.
/// Transient failures (timeout, network, outage) keep state unprocessed/processing
/// with the job in retry wait; they never produce uninterpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingState {
    /// Not yet attempted or waiting for an authorized destination/configuration.
    Unprocessed,
    /// Job is leased or in flight.
    Processing,
    /// Valid proposal was applied.
    Processed,
    /// Interpreter explicitly abstained; source remains searchable.
    Abstained,
    /// Processing ended without usable result (permanent failure, malformed response).
    Uninterpreted,
}

impl ProcessingState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingState::Unprocessed => "unprocessed",
            ProcessingState::Processing => "processing",
            ProcessingState::Processed => "processed",
            ProcessingState::Abstained => "abstained",
            ProcessingState::Uninterpreted => "uninterpreted",
        }
    }
}

impl fmt::Display for ProcessingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProcessingState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unprocessed" => Ok(ProcessingState::Unprocessed),
            "processing" => Ok(ProcessingState::Processing),
            "processed" => Ok(ProcessingState::Processed),
            "abstained" => Ok(ProcessingState::Abstained),
            "uninterpreted" => Ok(ProcessingState::Uninterpreted),
            _ => Err(ParseStateError(format!("Unknown ProcessingState: {}", s))),
        }
    }
}

/// Transcription state: audio transcription result per F01 contract.
/// Only applies to voice captures; text captures have state not_applicable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptionState {
    /// Not applicable: source is text, not audio.
    NotApplicable,
    /// Audio saved, on-device transcription not started.
    AudioPending,
    /// Transcription in progress.
    Transcribing,
    /// Transcript text attached; transcript is the searchable source.
    Transcribed,
    /// Language or model not available on device; audio retained.
    TranscriptionUnsupported,
    /// Transcription failed (unreadable audio); audio retained.
    TranscriptionFailed,
}

impl TranscriptionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptionState::NotApplicable => "not_applicable",
            TranscriptionState::AudioPending => "audio_pending",
            TranscriptionState::Transcribing => "transcribing",
            TranscriptionState::Transcribed => "transcribed",
            TranscriptionState::TranscriptionUnsupported => "transcription_unsupported",
            TranscriptionState::TranscriptionFailed => "transcription_failed",
        }
    }
}

impl fmt::Display for TranscriptionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TranscriptionState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_applicable" => Ok(TranscriptionState::NotApplicable),
            "audio_pending" => Ok(TranscriptionState::AudioPending),
            "transcribing" => Ok(TranscriptionState::Transcribing),
            "transcribed" => Ok(TranscriptionState::Transcribed),
            "transcription_unsupported" => Ok(TranscriptionState::TranscriptionUnsupported),
            "transcription_failed" => Ok(TranscriptionState::TranscriptionFailed),
            _ => Err(ParseStateError(format!(
                "Unknown TranscriptionState: {}",
                s
            ))),
        }
    }
}

/// Reason why reminder cannot be scheduled (for Unschedulable state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnschedulableReason {
    /// Scheduled time passed before scheduling attempt.
    TimeInPast,
    /// OS notification permission denied.
    PermissionDenied,
    /// System resource capacity exceeded.
    CapacityExceeded,
}

impl UnschedulableReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnschedulableReason::TimeInPast => "time_in_past",
            UnschedulableReason::PermissionDenied => "permission_denied",
            UnschedulableReason::CapacityExceeded => "capacity_exceeded",
        }
    }
}

impl fmt::Display for UnschedulableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for UnschedulableReason {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "time_in_past" => Ok(UnschedulableReason::TimeInPast),
            "permission_denied" => Ok(UnschedulableReason::PermissionDenied),
            "capacity_exceeded" => Ok(UnschedulableReason::CapacityExceeded),
            _ => Err(ParseStateError(format!(
                "Unknown UnschedulableReason: {}",
                s
            ))),
        }
    }
}

/// Reminder request state: what the user asked for per F01 contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderRequestState {
    /// User did not request a reminder.
    NotRequested,
    /// User requested reminder; single absolute instant was resolved.
    Resolved,
    /// Reminder requested but time is ambiguous or incomplete.
    NotScheduledYet,
    /// User requested repeating reminder (unsupported in M1).
    UnsupportedRecurrence,
    /// Resolved reminder cannot be installed.
    Unschedulable,
    /// User or system cancelled the reminder.
    Cancelled,
}

impl ReminderRequestState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderRequestState::NotRequested => "not_requested",
            ReminderRequestState::Resolved => "resolved",
            ReminderRequestState::NotScheduledYet => "not_scheduled_yet",
            ReminderRequestState::UnsupportedRecurrence => "unsupported_recurrence",
            ReminderRequestState::Unschedulable => "unschedulable",
            ReminderRequestState::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for ReminderRequestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReminderRequestState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_requested" => Ok(ReminderRequestState::NotRequested),
            "resolved" => Ok(ReminderRequestState::Resolved),
            "not_scheduled_yet" => Ok(ReminderRequestState::NotScheduledYet),
            "unsupported_recurrence" => Ok(ReminderRequestState::UnsupportedRecurrence),
            "unschedulable" => Ok(ReminderRequestState::Unschedulable),
            "cancelled" => Ok(ReminderRequestState::Cancelled),
            _ => Err(ParseStateError(format!(
                "Unknown ReminderRequestState: {}",
                s
            ))),
        }
    }
}

/// Reminder schedule state: native installation state per F01 contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderScheduleState {
    /// Reminder not scheduled (waiting for request resolution or installation).
    NotScheduled,
    /// Resolved reminder installation not yet confirmed (e.g., around boot or permission change).
    PendingSchedule,
    /// OS reports the request installed.
    Scheduled,
    /// Installation error; request stays resolved and can be retried.
    ScheduleFailed,
}

impl ReminderScheduleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderScheduleState::NotScheduled => "not_scheduled",
            ReminderScheduleState::PendingSchedule => "pending_schedule",
            ReminderScheduleState::Scheduled => "scheduled",
            ReminderScheduleState::ScheduleFailed => "schedule_failed",
        }
    }
}

impl fmt::Display for ReminderScheduleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReminderScheduleState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_scheduled" => Ok(ReminderScheduleState::NotScheduled),
            "pending_schedule" => Ok(ReminderScheduleState::PendingSchedule),
            "scheduled" => Ok(ReminderScheduleState::Scheduled),
            "schedule_failed" => Ok(ReminderScheduleState::ScheduleFailed),
            _ => Err(ParseStateError(format!(
                "Unknown ReminderScheduleState: {}",
                s
            ))),
        }
    }
}

/// Reminder delivery state: evidence from OS per F01 contract.
/// Never claims the user noticed; passage of due time does not change this state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderDeliveryState {
    /// No OS evidence of delivery.
    Unknown,
    /// OS reported notification delivered or presented.
    Delivered,
    /// User opened the app through the notification.
    Opened,
}

impl ReminderDeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderDeliveryState::Unknown => "unknown",
            ReminderDeliveryState::Delivered => "delivered",
            ReminderDeliveryState::Opened => "opened",
        }
    }
}

impl fmt::Display for ReminderDeliveryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReminderDeliveryState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unknown" => Ok(ReminderDeliveryState::Unknown),
            "delivered" => Ok(ReminderDeliveryState::Delivered),
            "opened" => Ok(ReminderDeliveryState::Opened),
            _ => Err(ParseStateError(format!(
                "Unknown ReminderDeliveryState: {}",
                s
            ))),
        }
    }
}

/// Reminder acknowledgment state: user's response to reminder per F01 contract.
/// Independent of delivery and of item completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderAcknowledgmentState {
    /// User has not acknowledged the reminder.
    NotAcknowledged,
    /// User explicitly acknowledged or dismissed the reminder.
    Acknowledged,
}

impl ReminderAcknowledgmentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderAcknowledgmentState::NotAcknowledged => "not_acknowledged",
            ReminderAcknowledgmentState::Acknowledged => "acknowledged",
        }
    }
}

impl fmt::Display for ReminderAcknowledgmentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReminderAcknowledgmentState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_acknowledged" => Ok(ReminderAcknowledgmentState::NotAcknowledged),
            "acknowledged" => Ok(ReminderAcknowledgmentState::Acknowledged),
            _ => Err(ParseStateError(format!(
                "Unknown ReminderAcknowledgmentState: {}",
                s
            ))),
        }
    }
}

/// Processing job retry status: distinguishes provider outage from never attempted.
/// Only present when ProcessingState is unprocessed/processing and a job exists in retry wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingJobStatus {
    /// Job is waiting to retry after a transient failure (e.g., outage).
    RetryingAfterTransient,
    /// Job is waiting for configuration or destination setup.
    AwaitingConfiguration,
}

impl ProcessingJobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingJobStatus::RetryingAfterTransient => "retrying_after_transient",
            ProcessingJobStatus::AwaitingConfiguration => "awaiting_configuration",
        }
    }
}

impl fmt::Display for ProcessingJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProcessingJobStatus {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "retrying_after_transient" => Ok(ProcessingJobStatus::RetryingAfterTransient),
            "awaiting_configuration" => Ok(ProcessingJobStatus::AwaitingConfiguration),
            _ => Err(ParseStateError(format!(
                "Unknown ProcessingJobStatus: {}",
                s
            ))),
        }
    }
}

/// Complete status snapshot for an item.
/// Distinguishes the separate independent facts: save, sync, processing, transcription,
/// reminder request, schedule, delivery, and acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemStatus {
    pub item_id: String,
    pub save_state: SaveState,
    pub sync_state: SyncState,
    pub processing_state: ProcessingState,
    pub transcription_state: TranscriptionState,
    pub processing_job_status: Option<ProcessingJobStatus>,
    pub reminder_request_state: Option<ReminderRequestState>,
    pub reminder_schedule_state: Option<ReminderScheduleState>,
    pub reminder_delivery_state: Option<ReminderDeliveryState>,
    pub reminder_acknowledgment_state: Option<ReminderAcknowledgmentState>,
    pub unschedulable_reason: Option<UnschedulableReason>,
}

impl ItemStatus {
    /// Load item status from the database.
    /// Reads item states from items table, reminder states from reminders table if present,
    /// and job/retry status from jobs table if processing is unprocessed/processing.
    pub fn load(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
    ) -> anyhow::Result<Option<ItemStatus>> {
        use rusqlite::OptionalExtension;

        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT save_state, sync_state, processing_state, transcription_state FROM items WHERE item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        let (save_str, sync_str, processing_str, transcription_str) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let save_state = save_str.parse::<SaveState>()?;
        let sync_state = sync_str.parse::<SyncState>()?;
        let processing_state = processing_str.parse::<ProcessingState>()?;
        let transcription_state = transcription_str.parse::<TranscriptionState>()?;

        let processing_job_status = if matches!(
            processing_state,
            ProcessingState::Unprocessed | ProcessingState::Processing
        ) {
            load_interpretation_job_status(tx, item_id)?
        } else {
            None
        };

        // Load reminder states if a reminder row exists for this item
        let reminder_row: Option<(String, String, String, String, Option<String>)> = tx
            .query_row(
                "SELECT request_state, schedule_state, delivery_state, acknowledgment_state, unschedulable_reason FROM reminders WHERE item_id = ?",
                [item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;

        let (
            reminder_request_state,
            reminder_schedule_state,
            reminder_delivery_state,
            reminder_acknowledgment_state,
            unschedulable_reason,
        ) = if let Some((req_str, sched_str, deliv_str, ack_str, reason_str)) = reminder_row {
            (
                Some(req_str.parse::<ReminderRequestState>()?),
                Some(sched_str.parse::<ReminderScheduleState>()?),
                Some(deliv_str.parse::<ReminderDeliveryState>()?),
                Some(ack_str.parse::<ReminderAcknowledgmentState>()?),
                reason_str
                    .as_deref()
                    .map(|s| s.parse::<UnschedulableReason>())
                    .transpose()?,
            )
        } else {
            (None, None, None, None, None)
        };

        match (reminder_request_state, unschedulable_reason) {
            (Some(ReminderRequestState::Unschedulable), None) => {
                anyhow::bail!("Unschedulable reminder for item {item_id} has no recorded reason")
            }
            (Some(request_state), Some(_))
                if request_state != ReminderRequestState::Unschedulable =>
            {
                anyhow::bail!(
                    "Reminder for item {item_id} records an unschedulable reason while {request_state}"
                )
            }
            _ => {}
        }

        Ok(Some(ItemStatus {
            item_id: item_id.to_string(),
            save_state,
            sync_state,
            processing_state,
            transcription_state,
            processing_job_status,
            reminder_request_state,
            reminder_schedule_state,
            reminder_delivery_state,
            reminder_acknowledgment_state,
            unschedulable_reason,
        }))
    }

    /// Check if this status claims explicit user attention.
    /// Only true if the user has explicitly acknowledged the reminder.
    /// Permission denial, expiration, ambiguity, outage, schedule failure, delivery, and opened
    /// without acknowledgment never claim attention.
    pub fn claims_user_attention(&self) -> bool {
        matches!(
            self.reminder_acknowledgment_state,
            Some(ReminderAcknowledgmentState::Acknowledged)
        )
    }
}

/// Failure reasons (F01 error classes plus the queue's own stop reason) that mean the job waits
/// for the user to fix configuration or authorization rather than for the provider to recover.
const CONFIGURATION_WAIT_REASONS: &[&str] =
    &["unauthorized", "unsupported", "unsupported_job_version"];

/// Derive the retry/configuration wait of the newest interpretation job from what the durable
/// queue persists: a retry after `fail_job_with_backoff` is a `queued` job with attempts made
/// and `next_attempt_at` set; an explicit configuration stop is a `failed` job (or a backed-off
/// job) carrying a configuration-class reason. A never-attempted job reports nothing.
fn load_interpretation_job_status(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
) -> anyhow::Result<Option<ProcessingJobStatus>> {
    use rusqlite::OptionalExtension;

    let job_row: Option<(String, Option<String>, i32, Option<String>)> = tx
        .query_row(
            "SELECT status, failure_reason, attempt_count, next_attempt_at FROM jobs \
             WHERE item_id = ? AND job_type = ? ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![item_id, JOB_TYPE_INTERPRET],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let Some((status_str, failure_reason, attempt_count, next_attempt_at)) = job_row else {
        return Ok(None);
    };
    let is_configuration_wait = failure_reason
        .as_deref()
        .is_some_and(|reason| CONFIGURATION_WAIT_REASONS.contains(&reason));

    Ok(match status_str.parse::<JobStatus>()? {
        JobStatus::Failed if is_configuration_wait => {
            Some(ProcessingJobStatus::AwaitingConfiguration)
        }
        JobStatus::Queued if attempt_count > 0 && next_attempt_at.is_some() => {
            if is_configuration_wait {
                Some(ProcessingJobStatus::AwaitingConfiguration)
            } else {
                Some(ProcessingJobStatus::RetryingAfterTransient)
            }
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_state_roundtrip() {
        for state in [SaveState::NotSaved, SaveState::SavedLocal] {
            let s = state.as_str();
            let parsed: SaveState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_sync_state_roundtrip() {
        let state = SyncState::NotConfigured;
        let s = state.as_str();
        let parsed: SyncState = s.parse().expect("should parse");
        assert_eq!(state, parsed);
    }

    #[test]
    fn test_sync_state_m1_baseline() {
        assert_eq!(SyncState::NotConfigured.as_str(), "not_configured");
        let parsed: SyncState = "not_configured".parse().expect("should parse");
        assert_eq!(parsed, SyncState::NotConfigured);
    }

    #[test]
    fn test_processing_state_roundtrip() {
        for state in [
            ProcessingState::Unprocessed,
            ProcessingState::Processing,
            ProcessingState::Processed,
            ProcessingState::Abstained,
            ProcessingState::Uninterpreted,
        ] {
            let s = state.as_str();
            let parsed: ProcessingState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_transcription_state_roundtrip() {
        for state in [
            TranscriptionState::NotApplicable,
            TranscriptionState::AudioPending,
            TranscriptionState::Transcribing,
            TranscriptionState::Transcribed,
            TranscriptionState::TranscriptionUnsupported,
            TranscriptionState::TranscriptionFailed,
        ] {
            let s = state.as_str();
            let parsed: TranscriptionState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_reminder_request_state_roundtrip() {
        for state in [
            ReminderRequestState::NotRequested,
            ReminderRequestState::Resolved,
            ReminderRequestState::NotScheduledYet,
            ReminderRequestState::UnsupportedRecurrence,
            ReminderRequestState::Unschedulable,
            ReminderRequestState::Cancelled,
        ] {
            let s = state.as_str();
            let parsed: ReminderRequestState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_reminder_schedule_state_roundtrip() {
        for state in [
            ReminderScheduleState::NotScheduled,
            ReminderScheduleState::PendingSchedule,
            ReminderScheduleState::Scheduled,
            ReminderScheduleState::ScheduleFailed,
        ] {
            let s = state.as_str();
            let parsed: ReminderScheduleState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_reminder_delivery_state_roundtrip() {
        for state in [
            ReminderDeliveryState::Unknown,
            ReminderDeliveryState::Delivered,
            ReminderDeliveryState::Opened,
        ] {
            let s = state.as_str();
            let parsed: ReminderDeliveryState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_reminder_acknowledgment_state_roundtrip() {
        for state in [
            ReminderAcknowledgmentState::NotAcknowledged,
            ReminderAcknowledgmentState::Acknowledged,
        ] {
            let s = state.as_str();
            let parsed: ReminderAcknowledgmentState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_unschedulable_reason_roundtrip() {
        for reason in [
            UnschedulableReason::TimeInPast,
            UnschedulableReason::PermissionDenied,
            UnschedulableReason::CapacityExceeded,
        ] {
            let s = reason.as_str();
            let parsed: UnschedulableReason = s.parse().expect("should parse");
            assert_eq!(reason, parsed);
        }
    }

    #[test]
    fn test_invalid_state_strings() {
        assert!("invalid".parse::<SaveState>().is_err());
        assert!("invalid".parse::<SyncState>().is_err());
        assert!("invalid".parse::<ProcessingState>().is_err());
        assert!("invalid".parse::<TranscriptionState>().is_err());
        assert!("invalid".parse::<ReminderRequestState>().is_err());
        assert!("invalid".parse::<ReminderScheduleState>().is_err());
        assert!("invalid".parse::<ReminderDeliveryState>().is_err());
        assert!("invalid".parse::<ReminderAcknowledgmentState>().is_err());
        assert!("invalid".parse::<UnschedulableReason>().is_err());
    }
}
