//! One isolated scratch store per evaluated case.
//!
//! The store is a migrated SQLite file in a fresh temporary directory, so every case starts from
//! the same state, nothing is shared between cases and no real store is ever opened. A candidate
//! is applied with the production application guard (`apply_interpretation_proposal`) and the
//! resulting durable state is read back through a new connection.

use chrono::{DateTime, Utc};
use ohand_core::domain::items::{load_item_state, LifecycleState};
use ohand_core::interpretation::apply::{
    apply_interpretation_proposal, record_interpretation_failure, ApplyOutcome, ReminderDisposition,
};
use ohand_core::interpretation::contracts::TextBasis;
use ohand_core::jobs::queue::{claim_job_with_lease, enqueue_job};
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::providers::contracts::ProviderFailure;
use ohand_core::reminders::state::{get_reminder, RequestState};
use ohand_core::store::captures::{save_capture, Capture};
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType, ItemType,
};
use ohand_core::store::schema::{Clock, Database};
use ohand_core::time::TimeContext;
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use tempfile::TempDir;

use crate::corpus::{CaseContext, EVALUATION_LOCALE};
use crate::EvaluationError;

const ROUTE_ID: &str = "route-1";
const LEASE_SECONDS: i64 = 60;

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// A UUID derived from the case label, so reruns of one case use identical identifiers.
fn stable_uuid(case_label: &str, purpose: &str) -> String {
    let digest = Sha256::digest(format!("{case_label}/{purpose}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// What a producer needs to build a candidate bound to the leased job.
#[derive(Debug, Clone)]
pub struct JobBinding {
    pub item_id: String,
    pub capture_id: String,
    pub request_version: String,
    pub proposal_id: String,
    pub source_revision: i32,
    pub text_basis: TextBasis,
    pub basis_text: String,
    pub time_context: TimeContext,
}

/// What reached the application guard.
pub enum Submission {
    Proposal(Box<ohand_core::interpretation::contracts::Proposal>),
    Failure(ProviderFailure),
}

/// The guard's verdict on a submission, without interpretation of whether it was right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum GuardOutcome {
    Applied { reminder: String },
    Abstained,
    FailedPermanently,
    RetryLater,
    Duplicate,
    Rejected { error: String },
}

/// Durable state after the guard ran, read through a new connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthoritativeState {
    pub item_type: Option<ItemType>,
    pub session_topic: Option<String>,
    pub lifecycle: String,
    pub processing_state: String,
    pub reminder: Option<StoredReminder>,
    pub raw_capture_intact: bool,
    pub correction_preserved: bool,
    /// Rows, per table, that belong to neither the evaluated item nor its capture. The scratch
    /// store holds exactly one item, so any such row means an item was created or another item
    /// was touched. Empty when nothing of the kind exists.
    pub foreign_records: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredReminder {
    pub request_state: String,
    pub resolved_instant: Option<DateTime<Utc>>,
    pub timezone_id: Option<String>,
    pub ambiguity_reason: Option<String>,
}

impl AuthoritativeState {
    pub fn is_active(&self) -> bool {
        self.lifecycle == lifecycle_name(LifecycleState::Active)
    }
}

fn lifecycle_name(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Active => "active",
        LifecycleState::Completed => "completed",
        LifecycleState::Cancelled => "cancelled",
        LifecycleState::Deleted => "deleted",
    }
}

fn request_state_name(state: &RequestState) -> String {
    match state {
        RequestState::NotRequested => "not_requested",
        RequestState::Resolved => "resolved",
        RequestState::NotScheduledYet => "not_scheduled_yet",
        RequestState::UnsupportedRecurrence => "unsupported_recurrence",
        RequestState::Unschedulable(_) => "unschedulable",
        RequestState::Cancelled => "cancelled",
    }
    .to_string()
}

fn reminder_disposition_name(disposition: &ReminderDisposition) -> String {
    match disposition {
        ReminderDisposition::NotRecorded => "not_recorded".to_string(),
        ReminderDisposition::NoExplicitIntent => "no_explicit_intent".to_string(),
        ReminderDisposition::CandidateRejected(_) => "candidate_rejected".to_string(),
        ReminderDisposition::ExistingKept => "existing_kept".to_string(),
        ReminderDisposition::Recorded(state) => format!("recorded_{}", state.as_str()),
    }
}

pub struct ScratchCase {
    _directory: TempDir,
    path: String,
    database: Database,
    context: CaseContext,
    binding: JobBinding,
    job_id: String,
    lease_attempt: i32,
    now: DateTime<Utc>,
}

impl ScratchCase {
    /// Create the store, the capture (with its correction, if any), the item and a leased
    /// on-device interpretation job for the current revision.
    pub fn open(case_label: &str, context: &CaseContext) -> Result<ScratchCase, EvaluationError> {
        let directory =
            tempfile::tempdir().map_err(|error| EvaluationError::Io(error.to_string()))?;
        let path = directory
            .path()
            .join("scratch.sqlite")
            .to_string_lossy()
            .into_owned();
        let now = context.capture_instant;
        std::fs::write(&path, migrated_template()?)
            .map_err(|error| EvaluationError::Io(error.to_string()))?;
        let mut database = open_database(&path, now)?;
        database
            .conn()
            .execute_batch("PRAGMA synchronous = OFF;")
            .map_err(|error| EvaluationError::Store(error.to_string()))?;

        let capture_id = stable_uuid(case_label, "capture");
        let item_id = stable_uuid(case_label, "item");
        let instant_text = now.to_rfc3339();
        let capture = Capture::new(
            capture_id.clone(),
            Some(context.raw_input.clone()),
            None,
            instant_text.clone(),
            context.timezone.clone(),
            context.utc_offset_seconds / 60,
            EVALUATION_LOCALE.to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            ROUTE_ID.to_string(),
            false,
            instant_text.clone(),
            None,
        )?;
        save_capture(&mut database, &capture)?;
        database
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
                 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, instant_text, instant_text],
            )
            .map_err(|error| EvaluationError::Store(error.to_string()))?;

        let (source_revision, text_basis) = match &context.user_correction {
            None => (0, TextBasis::Original { item_revision: 0 }),
            Some(corrected) => {
                let correction_id = stable_uuid(case_label, "correction");
                let event = Event::new(
                    correction_id.clone(),
                    item_id.clone(),
                    0,
                    EventType::Correction,
                    EventPayload::Correction(Correction {
                        kind: CorrectionKind::Text,
                        old_value: Some(context.raw_input.clone()),
                        new_value: corrected.clone(),
                    }),
                    instant_text.clone(),
                )
                .map_err(|error| EvaluationError::Store(error.to_string()))?;
                save_event(&mut database, &event, 0)
                    .map_err(|error| EvaluationError::Store(error.to_string()))?;
                (
                    1,
                    TextBasis::Correction {
                        correction_record_id: correction_id,
                        item_revision: 1,
                    },
                )
            }
        };

        let job_id = stable_uuid(case_label, "job");
        let request_version = stable_uuid(case_label, "request");
        enqueue_job(
            &mut database,
            job_id.clone(),
            item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            source_revision,
            None,
            Some(request_version.clone()),
            1,
            now,
        )
        .map_err(|error| EvaluationError::Store(error.to_string()))?;
        let claimed =
            claim_job_with_lease(&mut database, chrono::Duration::seconds(LEASE_SECONDS), now)
                .map_err(|error| EvaluationError::Store(error.to_string()))?
                .ok_or_else(|| {
                    EvaluationError::Store("the enqueued job was not claimable".into())
                })?;

        let binding = JobBinding {
            item_id,
            capture_id,
            request_version,
            proposal_id: stable_uuid(case_label, "proposal"),
            source_revision,
            text_basis,
            basis_text: context.basis_text().to_string(),
            time_context: context.time_context(),
        };
        Ok(ScratchCase {
            _directory: directory,
            path,
            database,
            context: context.clone(),
            binding,
            job_id,
            lease_attempt: claimed.attempt_count,
            now,
        })
    }

    pub fn binding(&self) -> &JobBinding {
        &self.binding
    }

    /// Submit to the production guard and return its verdict plus the durable state afterwards.
    pub fn submit(
        &mut self,
        submission: &Submission,
    ) -> Result<(GuardOutcome, AuthoritativeState), EvaluationError> {
        let result = match submission {
            Submission::Proposal(proposal) => apply_interpretation_proposal(
                &mut self.database,
                &self.job_id,
                self.lease_attempt,
                proposal,
                self.now,
            ),
            Submission::Failure(failure) => record_interpretation_failure(
                &mut self.database,
                &self.job_id,
                self.lease_attempt,
                failure,
                self.now,
            ),
        };
        let outcome = match result {
            Ok(ApplyOutcome::Applied { reminder, .. }) => GuardOutcome::Applied {
                reminder: reminder_disposition_name(&reminder),
            },
            Ok(ApplyOutcome::Abstained { .. }) => GuardOutcome::Abstained,
            Ok(ApplyOutcome::Failed { .. }) => GuardOutcome::FailedPermanently,
            Ok(ApplyOutcome::RetryLater) => GuardOutcome::RetryLater,
            Ok(ApplyOutcome::Duplicate) => GuardOutcome::Duplicate,
            Err(error) => GuardOutcome::Rejected {
                error: error.to_string(),
            },
        };
        Ok((outcome, self.read_state()?))
    }

    /// Direct access to the scratch connection, so a test can plant the row a regressed guard
    /// would have written and check that the harness notices.
    #[cfg(test)]
    pub(crate) fn connection_for_negative_control(&self) -> &rusqlite::Connection {
        self.database.conn()
    }

    /// The durable state without submitting anything.
    pub fn untouched_state(&self) -> Result<AuthoritativeState, EvaluationError> {
        self.read_state()
    }

    fn read_state(&self) -> Result<AuthoritativeState, EvaluationError> {
        let mut database = open_database(&self.path, self.now)?;
        let tx = database
            .immediate_transaction()
            .map_err(|error| EvaluationError::Store(error.to_string()))?;
        let store = |error: &dyn std::fmt::Display| EvaluationError::Store(error.to_string());
        let item = load_item_state(&tx, &self.binding.item_id)
            .map_err(|error| store(&error))?
            .ok_or_else(|| EvaluationError::Store("the scratch item disappeared".into()))?;
        let reminder = get_reminder(&tx, &self.binding.item_id).map_err(|error| store(&error))?;
        let processing_state: String = tx
            .query_row(
                "SELECT processing_state FROM items WHERE item_id = ?",
                [&self.binding.item_id],
                |row| row.get(0),
            )
            .map_err(|error| store(&error))?;
        let capture_text: Option<String> = tx
            .query_row(
                "SELECT text FROM captures WHERE capture_id = ?",
                [&self.binding.capture_id],
                |row| row.get(0),
            )
            .map_err(|error| store(&error))?;
        let stored_correction: Option<String> = tx
            .query_row(
                "SELECT new_value FROM corrections WHERE item_id = ? AND kind = 'text' \
                 ORDER BY revision DESC LIMIT 1",
                [&self.binding.item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| store(&error))?;
        let mut foreign_records = BTreeMap::new();
        for (table, key_column, own_key) in [
            ("items", "item_id", &self.binding.item_id),
            ("captures", "capture_id", &self.binding.capture_id),
            ("events", "item_id", &self.binding.item_id),
            ("corrections", "item_id", &self.binding.item_id),
            ("reminders", "item_id", &self.binding.item_id),
        ] {
            let foreign: i64 = tx
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {key_column} != ?"),
                    [own_key],
                    |row| row.get(0),
                )
                .map_err(|error| store(&error))?;
            if foreign > 0 {
                foreign_records.insert(table.to_string(), foreign as u64);
            }
        }
        Ok(AuthoritativeState {
            item_type: item.item_type,
            session_topic: item.session_topic,
            lifecycle: lifecycle_name(item.lifecycle_state).to_string(),
            processing_state,
            reminder: reminder
                .filter(|record| record.request_state != RequestState::NotRequested)
                .map(|record| StoredReminder {
                    request_state: request_state_name(&record.request_state),
                    resolved_instant: record.resolved_instant,
                    timezone_id: record.timezone_id,
                    ambiguity_reason: record.ambiguity_reason,
                }),
            raw_capture_intact: capture_text.as_deref() == Some(self.context.raw_input.as_str()),
            correction_preserved: stored_correction == self.context.user_correction,
            foreign_records,
        })
    }
}

/// The bytes of an empty, fully migrated store, built once so each case copies a file instead of
/// replaying every migration.
fn migrated_template() -> Result<&'static [u8], EvaluationError> {
    static TEMPLATE: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    TEMPLATE
        .get_or_init(|| {
            let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
            let path = directory
                .path()
                .join("template.sqlite")
                .to_string_lossy()
                .into_owned();
            drop(open_database(&path, DateTime::<Utc>::UNIX_EPOCH).map_err(|e| e.to_string())?);
            std::fs::read(&path).map_err(|error| error.to_string())
        })
        .as_deref()
        .map_err(|error| EvaluationError::Store(error.clone()))
}

fn open_database(path: &str, now: DateTime<Utc>) -> Result<Database, EvaluationError> {
    Database::open(path, Arc::new(FixedClock(now)))
        .map_err(|error| EvaluationError::Store(error.to_string()))
}
