//! Proposal validation and atomic application (I05).
//!
//! An interpretation result is only a candidate. [`apply_interpretation_proposal_in_tx`] checks it
//! against durable state inside one savepoint and, only when every check passes, records the
//! proposal, applies it through the D04 projection, derives the reminder request and completes
//! the interpretation job together. Any rejection rolls the savepoint back, so the item, its
//! reminder, its proposal history and the job are exactly as before, and a crash cannot leave an
//! applied proposal with an unfinished job (or the reverse).
//!
//! Checks, in order: the job is the running interpretation job for this item and revision and the
//! caller holds its current lease; the job is still authorized (V02/V03); the item is active,
//! belongs to the proposal's capture and is at the proposal's revision; the proposal's text basis
//! is the item's current effective text; then [`Proposal::validate`] on that text. Output that
//! fails semantic validation is recorded as a permanent `invalid_output` failure (the job ends `failed`, the item follows
//! the failure rules below); stale, unauthorized or mismatched results are
//! rejected with an error and record nothing.
//!
//! Processing-state rules: the first usable result decides the state (`processed` for an applied
//! proposal, `abstained` for an explicit abstention, `uninterpreted` for a permanent failure or
//! invalid output). A later abstention never replaces `processed`; a later failure never replaces
//! `processed` or `abstained`. Transient failures change nothing and are left to the queue's
//! retry backoff. No state here ever alters the captured source text, so every record stays
//! searchable, and an item without an applied type stays excluded from suggestions.

use crate::domain::items::{
    apply_proposal, load_item_state, ItemState, LifecycleState, ProposalApplicationError,
};
use crate::domain::status::{
    ProcessingState, ReminderAcknowledgmentState, ReminderDeliveryState, ReminderRequestState,
    ReminderScheduleState,
};
use crate::interpretation::contracts::{
    Proposal, ReminderProposal, TextBasis, TimeResolutionQuality,
};
use crate::jobs::queue::{complete_job_in_tx, get_job_internal, Job, JobStatus};
use crate::privacy::routing::{authorize_job, DenialReason, JOB_TYPE_INTERPRET};
use crate::providers::contracts::{ErrorClass, ProviderFailure};
use crate::store::schema::Database;
use crate::time::resolver::{ResolutionError, TimeContext, TimeResolver};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use thiserror::Error;
use uuid::Uuid;

/// Failure reason recorded on a job whose output failed semantic validation.
pub const INVALID_OUTPUT_REASON: &str = "invalid_output";

/// Why a result was rejected. A rejection leaves every durable record unchanged.
#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("storage error: {0}")]
    Storage(#[from] anyhow::Error),
    #[error("job {0} does not exist")]
    JobNotFound(String),
    #[error("job is a {job_type:?} job, not an interpretation job")]
    NotAnInterpretationJob { job_type: String },
    #[error("job is {status}, not running with this lease")]
    JobNotRunning { status: &'static str },
    #[error("lease attempt {presented} is not the job's current attempt {current}")]
    StaleLease { presented: i32, current: i32 },
    #[error("result {field} does not match the job")]
    JobBindingMismatch { field: &'static str },
    #[error("job is not authorized: {0}")]
    Unauthorized(DenialReason),
    #[error("item {0} does not exist")]
    ItemNotFound(String),
    #[error("item lifecycle is {0}; model output cannot be applied")]
    ItemNotActive(LifecycleState),
    #[error("result belongs to a different capture than the item")]
    CaptureMismatch,
    #[error(
        "result is stale: computed at revision {proposal_revision}, item is at {current_revision}"
    )]
    StaleRevision {
        proposal_revision: i32,
        current_revision: i32,
    },
    #[error("proposal text basis is not the item's current effective text")]
    TextBasisStale,
    #[error("item has no text to interpret")]
    NoSourceText,
    #[error("proposal id {0} was already recorded")]
    DuplicateProposalId(String),
    #[error("proposal rejected by item state: {0}")]
    Rejected(ProposalApplicationError),
}

/// How the reminder candidate of an applied proposal was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderDisposition {
    /// No reminder candidate, or the item is not an action and cannot carry one.
    NotRecorded,
    /// An existing resolved, unschedulable or cancelled reminder was left untouched.
    ExistingKept,
    /// Recorded with the given request state.
    Recorded(ReminderRequestState),
}

/// What an accepted result did.
#[derive(Debug)]
pub enum ApplyOutcome {
    /// The proposal was applied and the job completed.
    Applied {
        item: Box<ItemState>,
        reminder: ReminderDisposition,
    },
    /// The proposal was an explicit abstention; the job completed.
    Abstained { processing_state: ProcessingState },
    /// The job ended without a usable result and is `failed`.
    Failed { processing_state: ProcessingState },
    /// A transient failure: nothing changed; the caller backs the job off.
    RetryLater,
    /// The exact result was already recorded by an earlier call; nothing changed.
    Duplicate,
}

/// Apply a validated-at-the-boundary proposal and complete its job in one transaction.
pub fn apply_interpretation_proposal(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    proposal: &Proposal,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    let tx = db.immediate_transaction()?;
    let outcome = apply_interpretation_proposal_in_tx(&tx, job_id, lease_attempt, proposal, now)?;
    tx.commit().map_err(anyhow::Error::from)?;
    Ok(outcome)
}

/// Record a provider failure for an interpretation job in one transaction.
pub fn record_interpretation_failure(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    failure: &ProviderFailure,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    let tx = db.immediate_transaction()?;
    let outcome = record_interpretation_failure_in_tx(&tx, job_id, lease_attempt, failure, now)?;
    tx.commit().map_err(anyhow::Error::from)?;
    Ok(outcome)
}

const SAVEPOINT: &str = "interpretation_apply";

fn within_savepoint<T>(
    tx: &Transaction<'_>,
    body: impl FnOnce() -> Result<T, ApplyError>,
) -> Result<T, ApplyError> {
    tx.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))
        .map_err(anyhow::Error::from)?;
    match body() {
        Ok(value) => {
            tx.execute_batch(&format!("RELEASE {SAVEPOINT}"))
                .map_err(anyhow::Error::from)?;
            Ok(value)
        }
        Err(error) => {
            tx.execute_batch(&format!("ROLLBACK TO {SAVEPOINT}; RELEASE {SAVEPOINT}"))
                .map_err(anyhow::Error::from)?;
            Err(error)
        }
    }
}

/// Everything about the item and its capture that a result is checked against.
struct SourceContext {
    item: ItemState,
    processing_state: ProcessingState,
    capture_text: Option<String>,
    time_context: TimeContext,
}

enum JobGate {
    Proceed(Job),
    AlreadyRecorded,
}

/// Bind a result to its running job: type, lease, item and authorization. A job that already
/// finished is reported as `AlreadyRecorded` only when `already_recorded` confirms that this exact
/// result produced the finish; otherwise the stale caller is rejected.
fn gate_job(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    already_recorded: impl FnOnce(&Job) -> Result<bool, ApplyError>,
) -> Result<JobGate, ApplyError> {
    let job =
        get_job_internal(tx, job_id)?.ok_or_else(|| ApplyError::JobNotFound(job_id.into()))?;
    if job.job_type != JOB_TYPE_INTERPRET {
        return Err(ApplyError::NotAnInterpretationJob {
            job_type: job.job_type,
        });
    }
    if job.attempt_count == lease_attempt && already_recorded(&job)? {
        return Ok(JobGate::AlreadyRecorded);
    }
    if job.status != JobStatus::Running {
        return Err(ApplyError::JobNotRunning {
            status: job.status.as_str(),
        });
    }
    if job.attempt_count != lease_attempt {
        return Err(ApplyError::StaleLease {
            presented: lease_attempt,
            current: job.attempt_count,
        });
    }
    if let Some(denial) = authorize_job(tx, job_id)?.denial() {
        return Err(ApplyError::Unauthorized(denial));
    }
    Ok(JobGate::Proceed(job))
}

fn load_source_context(tx: &Transaction<'_>, job: &Job) -> Result<SourceContext, ApplyError> {
    let item = load_item_state(tx, &job.item_id)?
        .ok_or_else(|| ApplyError::ItemNotFound(job.item_id.clone()))?;
    if item.lifecycle_state != LifecycleState::Active {
        return Err(ApplyError::ItemNotActive(item.lifecycle_state));
    }
    if item.revision != job.source_revision {
        return Err(ApplyError::StaleRevision {
            proposal_revision: job.source_revision,
            current_revision: item.revision,
        });
    }

    type CaptureRow = (String, Option<String>, String, String, i32, String, String);
    let (processing_state_text, capture): (String, CaptureRow) = tx
        .query_row(
            "SELECT i.processing_state, c.capture_id, c.text, c.capture_instant, c.timezone_id,
                    c.utc_offset_minutes, c.locale, c.calendar
               FROM items i JOIN captures c ON c.capture_id = i.capture_id
              WHERE i.item_id = ?",
            [&job.item_id],
            |row| {
                Ok((
                    row.get(0)?,
                    (
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ),
                ))
            },
        )
        .map_err(anyhow::Error::from)?;
    let (_, capture_text, capture_instant, timezone, offset_minutes, locale, calendar) = capture;
    let processing_state = processing_state_text
        .parse::<ProcessingState>()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let reference_time = DateTime::parse_from_rfc3339(&capture_instant)
        .map_err(|error| anyhow::anyhow!("malformed capture_instant {capture_instant:?}: {error}"))?
        .with_timezone(&Utc);

    Ok(SourceContext {
        item,
        processing_state,
        capture_text,
        time_context: TimeContext {
            timezone,
            locale,
            reference_time,
            utc_offset_at_capture: offset_minutes.saturating_mul(60),
            calendar,
        },
    })
}

/// The text a proposal's span offsets refer to, which must be the item's current effective text:
/// the original capture text until a text correction exists, afterwards the latest correction.
fn resolve_basis_text(
    tx: &Transaction<'_>,
    item_id: &str,
    basis: &TextBasis,
    capture_text: Option<&str>,
) -> Result<String, ApplyError> {
    let latest_correction: Option<(String, String)> = tx
        .query_row(
            "SELECT correction_id, new_value FROM corrections
              WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(anyhow::Error::from)?;
    match (basis, latest_correction) {
        (TextBasis::Original { .. }, None) => capture_text
            .map(str::to_string)
            .ok_or(ApplyError::NoSourceText),
        (
            TextBasis::Correction {
                correction_record_id,
                ..
            },
            Some((current_id, current_text)),
        ) if *correction_record_id == current_id => Ok(current_text),
        _ => Err(ApplyError::TextBasisStale),
    }
}

/// Apply `proposal` for the running interpretation job `job_id` held under `lease_attempt`.
///
/// Atomic: on `Err` nothing is changed, even if the caller later commits `tx`.
pub fn apply_interpretation_proposal_in_tx(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    proposal: &Proposal,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    within_savepoint(tx, || {
        let job = match gate_job(tx, job_id, lease_attempt, |finished| {
            Ok(finished.status == JobStatus::Completed
                && proposal_recorded_for(tx, proposal, finished)?)
        })? {
            JobGate::AlreadyRecorded => return Ok(ApplyOutcome::Duplicate),
            JobGate::Proceed(job) => job,
        };

        if proposal.item_id != job.item_id {
            return Err(ApplyError::JobBindingMismatch { field: "item_id" });
        }
        if proposal.source_revision != job.source_revision {
            return Err(ApplyError::JobBindingMismatch {
                field: "source_revision",
            });
        }
        if job.request_version.as_deref() != Some(proposal.request_version.as_str()) {
            return Err(ApplyError::JobBindingMismatch {
                field: "request_version",
            });
        }

        let source = load_source_context(tx, &job)?;
        if proposal.capture_id != source.item.capture_id {
            return Err(ApplyError::CaptureMismatch);
        }
        let basis_text = resolve_basis_text(
            tx,
            &job.item_id,
            &proposal.text_basis,
            source.capture_text.as_deref(),
        )?;

        let proposal_already_stored: bool = tx
            .query_row(
                "SELECT 1 FROM proposals WHERE proposal_id = ?",
                [&proposal.proposal_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(anyhow::Error::from)?
            .is_some();
        if proposal_already_stored {
            return Err(ApplyError::DuplicateProposalId(
                proposal.proposal_id.clone(),
            ));
        }

        if proposal.validate(&basis_text).is_err() {
            return fail_job(
                tx,
                &job,
                &source,
                FailureDisposition::Permanent(INVALID_OUTPUT_REASON.to_string()),
                now,
            );
        }

        if proposal.abstention.is_some() {
            insert_proposal_row(
                tx,
                proposal,
                &source.item.capture_id,
                "abstained",
                true,
                now,
            )?;
            let processing_state = settle_processing_state(
                tx,
                &job.item_id,
                &source,
                ProcessingState::Abstained,
                now,
            )?;
            complete_job_in_tx(tx, job_id, lease_attempt)?;
            return Ok(ApplyOutcome::Abstained { processing_state });
        }

        insert_proposal_row(
            tx,
            proposal,
            &source.item.capture_id,
            "unapplied",
            false,
            now,
        )?;
        let item = apply_proposal(tx, &job.item_id, &proposal.proposal_id)
            .map_err(ApplyError::Rejected)?;
        let reminder = record_reminder(tx, proposal, &item, &basis_text, &source, now)?;
        settle_processing_state(tx, &job.item_id, &source, ProcessingState::Processed, now)?;
        complete_job_in_tx(tx, job_id, lease_attempt)?;
        Ok(ApplyOutcome::Applied {
            item: Box::new(item),
            reminder,
        })
    })
}

fn proposal_recorded_for(
    tx: &Transaction<'_>,
    proposal: &Proposal,
    job: &Job,
) -> Result<bool, ApplyError> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM proposals WHERE proposal_id = ? AND item_id = ? AND request_version IS ?",
            rusqlite::params![&proposal.proposal_id, &job.item_id, &job.request_version],
            |_| Ok(()),
        )
        .optional()
        .map_err(anyhow::Error::from)?
        .is_some())
}

fn insert_proposal_row(
    tx: &Transaction<'_>,
    proposal: &Proposal,
    capture_id: &str,
    applied_state: &str,
    abstained: bool,
    now: DateTime<Utc>,
) -> Result<(), ApplyError> {
    let (basis_kind, basis_id) = match &proposal.text_basis {
        TextBasis::Original { .. } => ("original", None),
        TextBasis::Correction {
            correction_record_id,
            ..
        } => ("correction", Some(correction_record_id.as_str())),
    };
    let reminder_json = proposal
        .reminder_proposal
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(anyhow::Error::from)?;
    let spans_json = proposal
        .source_spans
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(anyhow::Error::from)?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version,
                                text_basis_kind, text_basis_id, applied_state, proposal_type,
                                reminder_proposal, session_topic_proposal, source_spans, abstained,
                                request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &proposal.proposal_id,
            &proposal.item_id,
            capture_id,
            proposal.source_revision,
            proposal.schema_version,
            basis_kind,
            basis_id,
            applied_state,
            proposal.item_type.map(|item_type| item_type.as_str()),
            reminder_json,
            proposal
                .session_topic_proposal
                .as_ref()
                .map(|topic| topic.topic.trim().to_string()),
            spans_json,
            i32::from(abstained),
            &proposal.request_version,
            now.to_rfc3339(),
        ],
    )
    .map_err(anyhow::Error::from)?;
    Ok(())
}

/// Move the item to `wanted` unless a stronger prior result must be retained: `processed` is never
/// replaced by an abstention, and `processed`/`abstained` are never replaced by a failure.
/// Returns the state the item ends in.
fn settle_processing_state(
    tx: &Transaction<'_>,
    item_id: &str,
    source: &SourceContext,
    wanted: ProcessingState,
    now: DateTime<Utc>,
) -> Result<ProcessingState, ApplyError> {
    let retained = match (source.processing_state, wanted) {
        (
            ProcessingState::Processed,
            ProcessingState::Abstained | ProcessingState::Uninterpreted,
        ) => true,
        (ProcessingState::Abstained, ProcessingState::Uninterpreted) => true,
        _ => false,
    };
    if retained {
        return Ok(source.processing_state);
    }
    tx.execute(
        "UPDATE items SET processing_state = ?, updated_at = ? WHERE item_id = ?",
        rusqlite::params![wanted.as_str(), now.to_rfc3339(), item_id],
    )
    .map_err(anyhow::Error::from)?;
    Ok(wanted)
}

enum FailureDisposition {
    /// Permanent: no retry; the item becomes uninterpreted unless a prior result is retained.
    Permanent(String),
    /// Credential or capability problem: the job stops with this reason and the item keeps waiting
    /// as unprocessed for the user to fix configuration.
    ConfigurationWait(&'static str),
}

fn fail_job(
    tx: &Transaction<'_>,
    job: &Job,
    source: &SourceContext,
    disposition: FailureDisposition,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    let (reason, processing_state) = match &disposition {
        FailureDisposition::Permanent(reason) => (
            reason.as_str(),
            settle_processing_state(
                tx,
                &job.item_id,
                source,
                ProcessingState::Uninterpreted,
                now,
            )?,
        ),
        FailureDisposition::ConfigurationWait(reason) => {
            let state = if source.processing_state == ProcessingState::Processing {
                tx.execute(
                    "UPDATE items SET processing_state = ?, updated_at = ? WHERE item_id = ?",
                    rusqlite::params![
                        ProcessingState::Unprocessed.as_str(),
                        now.to_rfc3339(),
                        &job.item_id
                    ],
                )
                .map_err(anyhow::Error::from)?;
                ProcessingState::Unprocessed
            } else {
                source.processing_state
            };
            (*reason, state)
        }
    };
    let affected = tx
        .execute(
            "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL
              WHERE job_id = ? AND status = ? AND attempt_count = ?",
            rusqlite::params![
                JobStatus::Failed.as_str(),
                reason,
                &job.job_id,
                JobStatus::Running.as_str(),
                job.attempt_count
            ],
        )
        .map_err(anyhow::Error::from)?;
    if affected == 0 {
        return Err(ApplyError::StaleLease {
            presented: job.attempt_count,
            current: job.attempt_count,
        });
    }
    Ok(ApplyOutcome::Failed { processing_state })
}

fn failure_reason(failure: &ProviderFailure) -> Option<FailureDisposition> {
    match failure.class {
        ErrorClass::Transient | ErrorClass::Cancelled => None,
        ErrorClass::Unauthorized => Some(FailureDisposition::ConfigurationWait("unauthorized")),
        ErrorClass::Unsupported => Some(FailureDisposition::ConfigurationWait("unsupported")),
        ErrorClass::Permanent => {
            let kind = serde_json::to_value(failure.kind)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "permanent".to_string());
            Some(FailureDisposition::Permanent(kind))
        }
    }
}

/// Record a normalized provider failure for the running interpretation job.
///
/// Permanent failures end the job as `failed` and leave a first interpretation `uninterpreted`
/// (a later failure keeps a prior `processed` or `abstained` state and all applied facets).
/// Unauthorized and unsupported failures stop the job for configuration without changing the
/// item's result. Transient and cancelled failures change nothing and return `RetryLater`.
/// Atomic: on `Err` nothing is changed.
pub fn record_interpretation_failure_in_tx(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    failure: &ProviderFailure,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    within_savepoint(tx, || {
        let disposition = failure_reason(failure);
        let job = match gate_job(tx, job_id, lease_attempt, |finished| {
            Ok(finished.status == JobStatus::Failed
                && match &disposition {
                    Some(FailureDisposition::Permanent(reason)) => {
                        finished.failure_reason.as_deref() == Some(reason.as_str())
                    }
                    Some(FailureDisposition::ConfigurationWait(reason)) => {
                        finished.failure_reason.as_deref() == Some(*reason)
                    }
                    None => false,
                })
        })? {
            JobGate::AlreadyRecorded => return Ok(ApplyOutcome::Duplicate),
            JobGate::Proceed(job) => job,
        };
        let source = load_source_context(tx, &job)?;
        match disposition {
            None => Ok(ApplyOutcome::RetryLater),
            Some(disposition) => fail_job(tx, &job, &source, disposition, now),
        }
    })
}

/// Derive the reminder request of an applied proposal. A reminder needs an item that can carry an
/// obligation, and it is `resolved` only when the deterministic resolver, given the quoted source
/// phrase and the capture's own time context, produces exactly the one future instant the
/// proposal claims. Everything else is recorded as unscheduled with its reason, never guessed,
/// and never reduced to a one-shot.
fn record_reminder(
    tx: &Transaction<'_>,
    proposal: &Proposal,
    item: &ItemState,
    basis_text: &str,
    source: &SourceContext,
    now: DateTime<Utc>,
) -> Result<ReminderDisposition, ApplyError> {
    let Some(candidate) = &proposal.reminder_proposal else {
        return Ok(ReminderDisposition::NotRecorded);
    };
    if !item
        .item_type
        .map(|item_type| item_type.can_carry_obligation())
        .unwrap_or(false)
    {
        return Ok(ReminderDisposition::NotRecorded);
    }

    let existing: Option<(String, String, Option<String>, i64)> = tx
        .query_row(
            "SELECT reminder_id, request_state, resolved_instant, schedule_generation
               FROM reminders WHERE item_id = ?",
            [&item.item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(anyhow::Error::from)?;
    if let Some((_, state, _, _)) = &existing {
        let state = state
            .parse::<ReminderRequestState>()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        if matches!(
            state,
            ReminderRequestState::Resolved
                | ReminderRequestState::Unschedulable
                | ReminderRequestState::Cancelled
        ) {
            return Ok(ReminderDisposition::ExistingKept);
        }
    }

    let phrase = quoted_phrase(candidate, basis_text);
    let decision = decide_reminder(candidate, &phrase, &source.time_context);
    let (request_state, instant, ambiguity_reason, unsupported_reason, unschedulable_reason) =
        match decision {
            ReminderDecision::Resolved(instant) => (
                ReminderRequestState::Resolved,
                Some(instant),
                None,
                None,
                None,
            ),
            ReminderDecision::InPast(instant) => (
                ReminderRequestState::Unschedulable,
                Some(instant),
                None,
                None,
                Some("time_in_past"),
            ),
            ReminderDecision::NotScheduledYet(reason) => (
                ReminderRequestState::NotScheduledYet,
                None,
                Some(format!("{reason} (phrase: {phrase:?})")),
                None,
                None,
            ),
            ReminderDecision::UnsupportedRecurrence => (
                ReminderRequestState::UnsupportedRecurrence,
                None,
                None,
                Some(format!(
                    "recurring reminders are not supported (phrase: {phrase:?})"
                )),
                None,
            ),
        };

    let schedule_state = if request_state == ReminderRequestState::Resolved {
        ReminderScheduleState::PendingSchedule
    } else {
        ReminderScheduleState::NotScheduled
    };
    let instant_text = instant.map(|instant| instant.to_rfc3339());
    let timestamp = now.to_rfc3339();
    match existing {
        Some((reminder_id, _, previous_instant, generation)) => {
            let next_generation = if instant_text.is_some() && instant_text != previous_instant {
                generation + 1
            } else {
                generation
            };
            tx.execute(
                "UPDATE reminders SET request_state = ?, schedule_state = ?, resolved_instant = ?,
                        timezone_id = ?, ambiguity_reason = ?, unsupported_reason = ?,
                        unschedulable_reason = ?, schedule_generation = ?, updated_at = ?
                  WHERE reminder_id = ?",
                rusqlite::params![
                    request_state.as_str(),
                    schedule_state.as_str(),
                    instant_text,
                    &source.time_context.timezone,
                    ambiguity_reason,
                    unsupported_reason,
                    unschedulable_reason,
                    next_generation,
                    timestamp,
                    reminder_id,
                ],
            )
            .map_err(anyhow::Error::from)?;
        }
        None => {
            tx.execute(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state,
                        delivery_state, acknowledgment_state, resolved_instant, timezone_id,
                        ambiguity_reason, unsupported_reason, unschedulable_reason,
                        schedule_generation, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    Uuid::new_v4().to_string(),
                    &item.item_id,
                    request_state.as_str(),
                    schedule_state.as_str(),
                    ReminderDeliveryState::Unknown.as_str(),
                    ReminderAcknowledgmentState::NotAcknowledged.as_str(),
                    instant_text,
                    &source.time_context.timezone,
                    ambiguity_reason,
                    unsupported_reason,
                    unschedulable_reason,
                    i64::from(instant_text.is_some()),
                    timestamp,
                    timestamp,
                ],
            )
            .map_err(anyhow::Error::from)?;
        }
    }
    Ok(ReminderDisposition::Recorded(request_state))
}

fn quoted_phrase(candidate: &ReminderProposal, basis_text: &str) -> String {
    candidate
        .source_span
        .map(|span| {
            basis_text
                .chars()
                .skip(span.start)
                .take(span.end - span.start)
                .collect()
        })
        .unwrap_or_default()
}

enum ReminderDecision {
    Resolved(DateTime<Utc>),
    InPast(DateTime<Utc>),
    NotScheduledYet(String),
    UnsupportedRecurrence,
}

fn decide_reminder(
    candidate: &ReminderProposal,
    phrase: &str,
    time_context: &TimeContext,
) -> ReminderDecision {
    let resolution = match TimeResolver::resolve(phrase, time_context) {
        Ok(resolution) => resolution,
        Err(ResolutionError::UnsupportedRepeat(_)) => {
            return ReminderDecision::UnsupportedRecurrence
        }
        Err(ResolutionError::InvalidDateFormat(_)) => {
            return ReminderDecision::NotScheduledYet(
                "time phrase is not in a supported form".to_string(),
            )
        }
        Err(other) => return ReminderDecision::NotScheduledYet(other.to_string()),
    };
    let Some(resolved) = resolution.resolved_time else {
        return ReminderDecision::NotScheduledYet(
            resolution
                .ambiguity_reason
                .unwrap_or_else(|| "time is ambiguous".to_string()),
        );
    };
    if candidate.quality == TimeResolutionQuality::Ambiguous {
        return ReminderDecision::NotScheduledYet(
            "the interpreter reported the time as ambiguous".to_string(),
        );
    }
    let claimed = candidate
        .instant
        .as_deref()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|instant| instant.with_timezone(&Utc));
    if claimed != Some(resolved) {
        return ReminderDecision::NotScheduledYet(
            "the proposed instant is not what the source phrase resolves to".to_string(),
        );
    }
    if candidate.timezone_id.as_deref() != Some(time_context.timezone.as_str()) {
        return ReminderDecision::NotScheduledYet(
            "the proposed timezone differs from the capture's timezone".to_string(),
        );
    }
    if resolution.is_past {
        return ReminderDecision::InPast(resolved);
    }
    if resolution.is_ambiguous {
        return ReminderDecision::NotScheduledYet(
            resolution
                .ambiguity_reason
                .unwrap_or_else(|| "time is ambiguous".to_string()),
        );
    }
    ReminderDecision::Resolved(resolved)
}
