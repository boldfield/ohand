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
//! A text-correction basis names its correction by the correction event's UUID; the store keeps
//! that record as `<event id>-correction` and the two are matched by that rule. A reminder
//! candidate is recorded only when the source text itself asks to be reminded and the model's
//! instant agrees with the deterministic resolver; the write then goes through the N01 reminder
//! desired-state API in the same savepoint, so the schedule operation is atomic with the rest.
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
use crate::domain::status::{ProcessingState, ReminderRequestState};
use crate::interpretation::contracts::{
    Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
};
use crate::jobs::queue::{complete_job_in_tx, get_job_internal, Job, JobStatus};
use crate::privacy::routing::{authorize_job, DenialReason, JOB_TYPE_INTERPRET};
use crate::providers::contracts::{ErrorClass, ProviderFailure};
use crate::reminders::state::{
    apply_derived_request, DerivedReminderRequest, ReminderStateError, RequestState,
};
use crate::store::schema::{Clock, Database};
use crate::time::resolver::{TimeContext, TimeResolver};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use thiserror::Error;

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
    #[error("reminder state: {0}")]
    Reminder(ReminderStateError),
}

/// How the reminder candidate of an applied proposal was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderDisposition {
    /// No reminder candidate, or the item is not an action and cannot carry one.
    NotRecorded,
    /// The item's text never asks to be reminded (or the request is negated or quoted), so the
    /// candidate is dropped and no reminder exists.
    NoExplicitIntent,
    /// The candidate contradicts the deterministic resolver (different instant or timezone, or a
    /// time the resolver itself calls ambiguous); nothing is scheduled and no reminder is created.
    CandidateRejected(&'static str),
    /// The reminder was already committed, recurring or cancelled; derived output cannot replace
    /// it, so it was left exactly as it was.
    ExistingKept,
    /// Recorded through the reminder desired-state API with the given request state.
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

/// The store names a correction `<correction event id>-correction`, while the proposal contract
/// requires its correction record identity to be a UUID. The identity a request carries is
/// therefore the correction event's id, and it matches the stored record by that suffix rule.
fn correction_matches(proposal_correction_id: &str, stored_correction_id: &str) -> bool {
    stored_correction_id.strip_suffix("-correction") == Some(proposal_correction_id)
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
        ) if correction_matches(correction_record_id, &current_id) => Ok(current_text),
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
    let retained = matches!(
        (source.processing_state, wanted),
        (
            ProcessingState::Processed,
            ProcessingState::Abstained | ProcessingState::Uninterpreted,
        ) | (ProcessingState::Abstained, ProcessingState::Uninterpreted)
    );
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
        let status = get_job_internal(tx, &job.job_id)?
            .map(|current| current.status.as_str())
            .unwrap_or("missing");
        return Err(ApplyError::JobNotRunning { status });
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

/// Derive the reminder request of an applied proposal.
///
/// A candidate is only a claim. It is dropped unless the item can carry an obligation and the
/// source text itself explicitly asks to be reminded ([`states_reminder_intent`]). The model's
/// instant and timezone must equal what the deterministic resolver produces from the quoted
/// phrase; a contradiction creates nothing. Once it agrees, the write goes through the reminder
/// desired-state API (N01), which re-resolves the phrase against the capture context, refuses to
/// reduce text with a repeat marker to a one-shot, never replaces a committed time, and records
/// the schedule operation in the same transaction. Unparseable, ambiguous or recurring phrases
/// are stored there as not scheduled with the phrase kept.
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
    if !states_reminder_intent(basis_text, candidate.source_span) {
        return Ok(ReminderDisposition::NoExplicitIntent);
    }

    let phrase = quoted_phrase(candidate, basis_text);
    if let Some(contradiction) = candidate_contradiction(candidate, &phrase, &source.time_context) {
        return Ok(ReminderDisposition::CandidateRejected(contradiction));
    }

    let request = DerivedReminderRequest {
        item_id: item.item_id.clone(),
        source_revision: item.revision,
        phrase,
    };
    match apply_derived_request(tx, &FixedClock(now), &request) {
        Ok(record) => Ok(ReminderDisposition::Recorded(match record.request_state {
            RequestState::NotRequested => ReminderRequestState::NotRequested,
            RequestState::Resolved => ReminderRequestState::Resolved,
            RequestState::NotScheduledYet => ReminderRequestState::NotScheduledYet,
            RequestState::UnsupportedRecurrence => ReminderRequestState::UnsupportedRecurrence,
            RequestState::Unschedulable(_) => ReminderRequestState::Unschedulable,
            RequestState::Cancelled => ReminderRequestState::Cancelled,
        })),
        Err(
            ReminderStateError::RecurrenceProtected
            | ReminderStateError::CommittedTimeProtected
            | ReminderStateError::ReminderCancelled,
        ) => Ok(ReminderDisposition::ExistingKept),
        Err(ReminderStateError::FabricatedDeadline(_) | ReminderStateError::Resolution(_)) => Ok(
            ReminderDisposition::CandidateRejected("the quoted phrase is not a usable time"),
        ),
        Err(other) => Err(ApplyError::Reminder(other)),
    }
}

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
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

/// Why the model's claim cannot be trusted, or `None` when it agrees with the resolver or the
/// resolver itself declines to produce one instant (ambiguous, unparseable or recurring phrases,
/// which the reminder API stores as not scheduled without using the claim).
fn candidate_contradiction(
    candidate: &ReminderProposal,
    phrase: &str,
    time_context: &TimeContext,
) -> Option<&'static str> {
    let resolution = TimeResolver::resolve(phrase, time_context).ok()?;
    let resolved = resolution.resolved_time?;
    if candidate.quality == TimeResolutionQuality::Ambiguous {
        return Some("the interpreter reported the time as ambiguous");
    }
    let claimed = candidate
        .instant
        .as_deref()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|instant| instant.with_timezone(&Utc));
    if claimed != Some(resolved) {
        return Some("the proposed instant is not what the source phrase resolves to");
    }
    if candidate.timezone_id.as_deref() != Some(time_context.timezone.as_str()) {
        return Some("the proposed timezone differs from the capture's timezone");
    }
    if resolution.is_ambiguous && !resolution.is_past {
        return Some("the resolver reports the time as ambiguous");
    }
    None
}

struct IntentToken {
    text: String,
    quoted: bool,
    clause_break: bool,
    start: usize,
    end: usize,
}

const REMINDER_CUES: &[&[&str]] = &[
    &["remind", "me"],
    &["set", "a", "reminder"],
    &["set", "reminder"],
    &["add", "a", "reminder"],
    &["alert", "me"],
    &["notify", "me"],
    &["ping", "me"],
];
const NEGATING_WORDS: &[&str] = &[
    "not", "no", "never", "dont", "don't", "doesnt", "doesn't", "didnt", "didn't", "wont", "won't",
    "cant", "can't", "cannot", "without", "if", "said", "says", "think", "thinks", "thought",
    "believe", "believes", "believed", "told", "tells", "hope", "hopes", "hoped",
];
/// Words that, between a reminder cue and the quoted time, exclude that time from the request
/// ("remind me except on ...") or attach it to something else ("remind me to call Bob who called
/// on ...").
const EXCLUDING_WORDS: &[&str] = &[
    "except",
    "but",
    "unless",
    "instead",
    "besides",
    "excluding",
    "other",
    "rather",
    "who",
    "whom",
    "whose",
    "which",
    "that",
];
/// Words that, directly after a reminder cue, ask to recall or explain something ("remind me why
/// the quote expires on ...", "you remind me of ...") rather than to be notified at a time. A
/// date in such a request belongs to the referenced fact, not to a notification.
const RECALL_WORDS: &[&str] = &[
    "why", "what", "whether", "how", "where", "who", "which", "of", "about", "that", "if",
];
const COMPLETED_WORDS: &[&str] = &[
    "already",
    "had",
    "have",
    "has",
    "did",
    "was",
    "were",
    "been",
    "previously",
    "earlier",
];
const MODAL_WORDS: &[&str] = &[
    "will", "would", "can", "could", "shall", "should", "may", "might", "must", "do", "does",
];
const FIRST_PERSON_REQUESTERS: &[&str] = &["i'll", "i'd", "we'll", "we'd", "you'll", "you'd"];
const SUBJECT_PRONOUNS: &[&str] = &["i", "you", "we"];
const FIRST_PERSON_SUBJECTS: &[&str] = &["i", "we", "i'll", "i'd", "we'll", "we'd"];
const FIRST_PERSON_PROGRESSIVE: &[&str] = &["i'm", "im", "we're"];
const POLITENESS_WORDS: &[&str] = &[
    "please", "pls", "kindly", "and", "then", "also", "hey", "ok", "okay", "now", "just",
];
/// Verbs that, followed by "to", carry the speaker's own request ("I need to", "I'd like to").
/// Only their first-person forms are listed: "Bob wants to" or "she needs to" is not a request.
const INTENTION_VERBS: &[&str] = &["need", "want", "like", "love"];

fn tokenize_intent_text(text: &str) -> Vec<IntentToken> {
    let characters: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut word_start = 0;
    let mut quoted = false;
    let flush = |word: &mut String,
                 start: usize,
                 end: usize,
                 quoted: bool,
                 tokens: &mut Vec<IntentToken>| {
        if !word.is_empty() {
            tokens.push(IntentToken {
                text: std::mem::take(word),
                quoted,
                clause_break: false,
                start,
                end,
            });
        }
    };
    for (index, character) in characters.iter().copied().enumerate() {
        let between_digits = index > 0
            && characters[index - 1].is_ascii_digit()
            && characters
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_digit());
        match character {
            '"' | '\u{201c}' | '\u{201d}' => {
                flush(&mut word, word_start, index, quoted, &mut tokens);
                quoted = !quoted;
            }
            '\'' | '\u{2019}' => {
                if word.is_empty() {
                    word_start = index;
                }
                word.push('\'');
            }
            '.' | ',' | ':' if between_digits => word.push(character),
            '.' | ',' | ';' | ':' | '!' | '?' | '\n' | '(' | ')' => {
                flush(&mut word, word_start, index, quoted, &mut tokens);
                tokens.push(IntentToken {
                    text: String::new(),
                    quoted,
                    clause_break: true,
                    start: index,
                    end: index + 1,
                });
            }
            other if other.is_alphanumeric() => {
                if word.is_empty() {
                    word_start = index;
                }
                word.extend(other.to_lowercase());
            }
            _ => flush(&mut word, word_start, index, quoted, &mut tokens),
        }
    }
    flush(&mut word, word_start, characters.len(), quoted, &mut tokens);
    tokens
}

/// Whether `text` itself asks for a reminder: a documented cue ("remind me", "set a reminder",
/// "alert/notify/ping me") outside quotation marks, as a present-tense first-person request (no
/// negating, reported-speech or opinion word, no completed-work word such as "already", no
/// third-party subject) anywhere earlier in the same clause. A cue followed directly by a recall
/// word ("remind me why/what/of/about ...") asks for information, not a notification.
///
/// With a `time_span` (character offsets of the phrase the model quoted), the request must also
/// govern that phrase: the span starts after the cue with no clause break in between, and no
/// negating or excluding word ("not", "except", "who") stands between the cue and the span. The
/// model's
/// candidate is never evidence of intent, and its choice of span cannot attach an unrelated date
/// ("the quote expires 2026-01-16 09:00:00") to a request made elsewhere in the text.
fn states_reminder_intent(text: &str, time_span: Option<SourceSpan>) -> bool {
    let tokens = tokenize_intent_text(text);
    (0..tokens.len()).any(|start| {
        REMINDER_CUES.iter().any(|cue| {
            let Some(window) = tokens.get(start..start + cue.len()) else {
                return false;
            };
            let matches_cue = window.iter().zip(cue.iter()).all(|(token, expected)| {
                !token.quoted && !token.clause_break && token.text == *expected
            });
            if !matches_cue || !is_present_first_person_request(&tokens, start, cue[0]) {
                return false;
            }
            let cue_end = window[cue.len() - 1].end;
            let recalls = tokens.get(start + cue.len()).is_some_and(|next| {
                !next.clause_break && RECALL_WORDS.contains(&next.text.as_str())
            });
            if recalls {
                return false;
            }
            time_span.is_none_or(|span| {
                span.start >= cue_end
                    && !tokens.iter().any(|token| {
                        token.start >= cue_end
                            && token.start < span.start
                            && (token.clause_break
                                || NEGATING_WORDS.contains(&token.text.as_str())
                                || EXCLUDING_WORDS.contains(&token.text.as_str()))
                    })
            })
        })
    })
}

fn is_present_first_person_request(tokens: &[IntentToken], cue_start: usize, verb: &str) -> bool {
    let clause_words: Vec<&str> = tokens[..cue_start]
        .iter()
        .rev()
        .take_while(|token| !token.clause_break)
        .map(|token| token.text.as_str())
        .collect();
    if clause_words
        .iter()
        .any(|word| NEGATING_WORDS.contains(word) || COMPLETED_WORDS.contains(word))
    {
        return false;
    }
    match skip_politeness(&clause_words) {
        [] => true,
        ["to", governing @ ..] => is_first_person_intention(governing),
        ["wanna", subject @ ..] => is_first_person_subject(subject),
        ["gonna", subject @ ..] => is_first_person_progressive(subject),
        [previous, ..] if FIRST_PERSON_REQUESTERS.contains(previous) => true,
        [modal, subject @ ..] if MODAL_WORDS.contains(modal) => subject
            .first()
            .is_none_or(|subject| SUBJECT_PRONOUNS.contains(subject)),
        [pronoun, before @ ..] if SUBJECT_PRONOUNS.contains(pronoun) => {
            let inverted_question = before
                .first()
                .is_some_and(|word| MODAL_WORDS.contains(word));
            inverted_question || (*pronoun == "you" && verb != "set" && verb != "add")
        }
        _ => false,
    }
}

/// Clause words (nearest first) before an infinitive cue ("... to remind me"): the governing verb
/// must be the speaker's own need or intention, optionally addressed to the assistant ("I need
/// you to"). Third-party subjects ("they need to", "Bob is going to"), third-person verb forms
/// ("Bob wants to"), other verbs ("Bob promised to") and delegation to someone else ("I told Bob
/// to") are not requests.
fn is_first_person_intention(words: &[&str]) -> bool {
    let words = match words {
        ["you", governing @ ..] => governing,
        other => other,
    };
    match words {
        [verb, subject @ ..] if INTENTION_VERBS.contains(verb) => is_first_person_subject(subject),
        ["going", subject @ ..] => is_first_person_progressive(subject),
        _ => false,
    }
}

/// Words before a first-person intention verb: none (a note such as "need to set a reminder"),
/// "I"/"we" (possibly contracted with a modal), or a modal after "I"/"we" ("I will need to").
fn is_first_person_subject(words: &[&str]) -> bool {
    match skip_politeness(words) {
        [] => true,
        [subject, ..] if FIRST_PERSON_SUBJECTS.contains(subject) => true,
        [modal, subject, ..] => {
            MODAL_WORDS.contains(modal) && FIRST_PERSON_SUBJECTS.contains(subject)
        }
        _ => false,
    }
}

/// Words before "going to"/"gonna": "I'm", "we're", "I am" or "we are".
fn is_first_person_progressive(words: &[&str]) -> bool {
    match words {
        [subject, ..] if FIRST_PERSON_PROGRESSIVE.contains(subject) => true,
        ["am", "i", ..] | ["are", "we", ..] => true,
        _ => false,
    }
}

fn skip_politeness<'slice, 'word>(words: &'slice [&'word str]) -> &'slice [&'word str] {
    let skipped = words
        .iter()
        .take_while(|word| POLITENESS_WORDS.contains(word))
        .count();
    &words[skipped..]
}
