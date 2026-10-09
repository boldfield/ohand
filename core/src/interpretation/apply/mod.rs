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
/// Plain negations, conditions and reporting or opinion words that, before a reminder cue or
/// between the cue and the quoted time, show the text does not make the request itself. Negative
/// contractions ("don't", "didn't", "isn't") are matched by [`is_negative_contraction`] rather
/// than listed, so this list and [`RETRACTING_NEGATIONS`] agree on every one of them.
const NEGATING_WORDS: &[&str] = &[
    "not", "no", "never", "cannot", "without", "if", "said", "says", "think", "thinks", "thought",
    "believe", "believes", "believed", "told", "tells", "hope", "hopes", "hoped",
];
/// Contracted negative auxiliaries typed without their apostrophe ("dont", "didnt", "isnt").
/// With the apostrophe, straight or curly, every such contraction ends in "n't" after
/// tokenization and needs no entry here.
const APOSTROPHE_LESS_NEGATIONS: &[&str] = &[
    "dont", "doesnt", "didnt", "wont", "cant", "isnt", "arent", "wasnt", "werent", "aint",
    "shouldnt", "wouldnt", "couldnt", "mustnt", "neednt", "shant", "hasnt", "havent", "hadnt",
];

/// Whether a lowercased token is a negative auxiliary contraction: any word ending in "n't"
/// (the tokenizer folds a curly apostrophe to a straight one) or an apostrophe-less spelling of
/// one. "didn't", a curly-apostrophe "didn’t", "didnt", "isn't", "aren't", "wasn't" and
/// "weren't" all match; "cannot" and the bare "not" are listed words instead.
fn is_negative_contraction(word: &str) -> bool {
    word.ends_with("n't") || APOSTROPHE_LESS_NEGATIONS.contains(&word)
}

/// Whether a token is a [`NEGATING_WORDS`] entry or a negative contraction.
fn is_negating_word(word: &str) -> bool {
    NEGATING_WORDS.contains(&word) || is_negative_contraction(word)
}

/// Whether a token is a [`RETRACTING_NEGATIONS`] entry or a negative contraction.
fn is_retracting_negation(word: &str) -> bool {
    RETRACTING_NEGATIONS.contains(&word) || is_negative_contraction(word)
}
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
/// Words that, after a reminder cue, ask to recall or explain something ("remind me why the quote
/// expires on ...", "remind me again of the meeting on ...") rather than to be notified at a time.
/// A date in such a request belongs to the referenced fact, not to a notification. They
/// disqualify the request whether they stand directly after the cue or anywhere between the cue
/// and the quoted time, so an intervening modifier ("again", "exactly", "once more") cannot hide
/// them.
const RECALL_WORDS: &[&str] = &[
    "why", "what", "whether", "how", "where", "who", "which", "of", "about", "that", "if",
];
/// Past-tense verbs that, between a reminder cue and the quoted time, show the date describes an
/// event that already took place ("remind me when the quote expired on ...") and therefore cannot
/// be a future notification time.
const PAST_EVENT_WORDS: &[&str] = &[
    "expired", "happened", "occurred", "ended", "started", "began", "finished", "closed", "passed",
    "arrived", "left", "went", "came", "ago",
];
/// Idioms without a verb that withdraw a request made earlier in the same capture ("..., never
/// mind", "..., on second thought").
const RETRACTION_IDIOMS: &[&[&str]] = &[
    &["never", "mind"],
    &["nevermind"],
    &["on", "second", "thought"],
    &["second", "thoughts"],
];
/// Verbs that withdraw an earlier request whatever their object is ("forget it", "forget about
/// it", "skip the reminder", a bare "cancel" or "stop", "delete that"). Listing exact verb-object pairs
/// would let every small rewording through, so the verb alone decides. The only exceptions are a
/// negated verb ("don't forget the ladder" keeps the request) and an infinitive ("to cancel the
/// subscription" is what the reminder is for).
const RETRACTION_VERBS: &[&str] = &[
    "forget",
    "forgetting",
    "cancel",
    "cancelled",
    "canceled",
    "cancelling",
    "canceling",
    "skip",
    "skipping",
    "stop",
    "scratch",
    "strike",
    "ignore",
    "disregard",
    "delete",
    "remove",
    "undo",
    "drop",
    "nix",
    "withdraw",
    "retract",
    "rescind",
];
/// Two-part withdrawals whose parts may be separated by other words of the same clause: "I take
/// that back", "I changed my mind", "I'll remember on my own / by myself".
const RETRACTION_PAIRS: &[(&[&str], &[&str])] = &[
    (&["take", "took", "taking"], &["back"]),
    (&["change", "changed", "changing"], &["mind"]),
    (
        &["remember", "handle", "manage"],
        &["own", "myself", "ourselves"],
    ),
];
/// Plain negations (not reporting or opinion words) that withdraw an earlier request when they
/// govern a reminder word ("actually don't remind me") or close their clause ("..., actually
/// don't"). Negative contractions in every tense and person ("don't", "didn't", "isn't",
/// "wasn't", with or without the apostrophe) are matched by [`is_negative_contraction`] through
/// [`is_retracting_negation`] rather than listed, so "I didn't want that reminder" and "it isn't
/// needed" withdraw exactly as "I did not want that reminder" and "it is not needed" do.
const RETRACTING_NEGATIONS: &[&str] = &["not", "no", "never", "cannot", "nope", "nah"];
/// Words a retracting negation may govern: the reminder itself or the act of being reminded.
const RETRACTION_TARGETS: &[&str] = &[
    "remind",
    "reminding",
    "reminder",
    "reminders",
    "set",
    "add",
    "alert",
    "alerts",
    "notify",
    "notification",
    "notifications",
    "ping",
    "schedule",
    "need",
    "bother",
    "worry",
];
/// Words that may sit between a retracting negation and the word it governs ("don't actually
/// remind me", "don't you remind me").
const RETRACTION_FILLERS: &[&str] = &[
    "actually", "really", "please", "pls", "kindly", "ever", "even", "just", "then", "now",
    "after", "all", "you", "ok", "okay", "wait", "um", "uh", "hmm",
];
/// Adjectives that say the reminder is unnecessary. A retracting negation governing one of them
/// withdraws the request ("not needed", "no longer necessary", "it's not required").
const NEEDLESS_WORDS: &[&str] = &[
    "needed",
    "necessary",
    "required",
    "relevant",
    "applicable",
    "wanted",
];
/// Verbs of wanting or needing: every inflection of want, need, desire and wish, the exact verbs
/// the bounded I05 grammar lists, plus care and interested. A retracting negation governing one
/// of them withdraws the request when the same clause says the wish has ended ("I no longer want
/// that reminder", "I don't need it anymore") or when the verb's own object is the reminder ("I
/// don't want that reminder", "I don't desire that reminder", "I don't wish to be reminded");
/// "I don't want to miss it" keeps the request. "need" itself is also a [`RETRACTION_TARGETS`]
/// entry, so "don't need" withdraws without either.
const DESIRE_WORDS: &[&str] = &[
    "want",
    "wants",
    "wanted",
    "wanting",
    "need",
    "needs",
    "needed",
    "needing",
    "desire",
    "desires",
    "desired",
    "desiring",
    "wish",
    "wishes",
    "wished",
    "wishing",
    "care",
    "interested",
];
/// Nouns and verb forms naming the reminder or the act of being reminded. A negated desire verb
/// whose object (after any [`OBJECT_LEAD_INS`]) is one of these withdraws the request.
const REMINDER_OBJECT_WORDS: &[&str] = &[
    "reminder",
    "reminders",
    "remind",
    "reminded",
    "reminding",
    "alert",
    "alerts",
    "alerted",
    "notification",
    "notifications",
    "notified",
    "ping",
    "pinged",
    "nudge",
];
/// Words that may stand between a desire verb and the object it governs: determiners ("that
/// reminder", "the reminder", "any reminders"), the preposition of "wish for" and "care for"
/// ("don't wish for that reminder"), and the links of a passive or delegated infinitive ("to be
/// reminded", "you to remind me"). A content word such as "miss" in "to miss it" is the object
/// itself and is not skipped.
const OBJECT_LEAD_INS: &[&str] = &[
    "that", "this", "the", "a", "an", "any", "my", "our", "such", "another", "those", "these",
    "you", "to", "be", "get", "for",
];
/// Words that say a wish or need has ended when they follow a retracting negation ("no longer",
/// "not anymore", "don't want it any more").
const ENDED_WORDS: &[&str] = &["longer", "anymore"];
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
/// govern that phrase: the span starts after the cue with no clause break in between, and every
/// word between the cue and the span is checked. A negating or excluding word ("not", "except",
/// "who") detaches the time from the request; a recall word ("why", "what", "of", "about")
/// anywhere in that segment, even after a modifier such as "again" or "once more", makes the date
/// part of the fact being recalled; a completed-work or past-tense word ("already", "expired",
/// "happened") shows the date lies in the past. The model's candidate is never evidence of
/// intent, and its choice of span cannot attach an unrelated date ("the quote expires 2026-01-16
/// 09:00:00") to a request made elsewhere in the text.
///
/// The rest of the capture after the quoted time (or after the cue, without a span) must not
/// withdraw the request ([`retracted_after`]): "remind me to call the roofer 2026-01-16 09:00:00,
/// actually don't remind me" ends with the intent not to schedule, so nothing is scheduled.
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
            let recalls = tokens.get(start + cue.len()).is_some_and(|next| {
                !next.clause_break && RECALL_WORDS.contains(&next.text.as_str())
            });
            if recalls {
                return false;
            }
            let cue_end = window[cue.len() - 1].end;
            let governs = time_span.is_none_or(|span| request_governs_span(&tokens, cue_end, span));
            governs && !retracted_after(&tokens, time_span.map_or(cue_end, |span| span.end))
        })
    })
}

/// Whether the capture withdraws the request after character offset `after` (the end of the
/// quoted time, or of the cue when no time was quoted). A withdrawal is, outside quotation marks:
/// a retraction idiom ("never mind", "on second thought"); a retraction verb with any object or
/// none ("forget about it", "skip the reminder", "cancel", "delete that"), unless it is negated
/// ("don't forget the ladder") or an infinitive naming what the reminder is for ("to cancel the
/// subscription"); a two-part construction within one clause ("I take that back", "I changed my
/// mind", "I'll remember on my own"); or a plain negation that governs a reminder word ("actually
/// don't remind me", "no reminder", "don't bother"), a needless adjective ("not needed", "not
/// necessary", "no longer necessary") or a wanting verb that either has the reminder as its object
/// ("I don't want that reminder", "I don't need the reminder") or whose clause says the wish has
/// ended ("I no longer want that reminder", "I don't need it anymore"), possibly through a filler
/// ("don't actually remind me"), or that closes its clause ("..., actually don't"). A negation
/// governing anything else ("I don't want to miss it", "it's not urgent") keeps the request.
/// This is the bounded withdrawal grammar recorded for I05 in
/// `docs/features/m1-task-refinement.md`; phrasings outside it belong to I05b. Rejecting too much
/// is safe: the reminder stays unscheduled and the original intention is kept with the item.
fn retracted_after(tokens: &[IntentToken], after: usize) -> bool {
    let later: Vec<&IntentToken> = tokens.iter().filter(|token| token.start >= after).collect();
    (0..later.len()).any(|index| {
        let token = later[index];
        if token.quoted || token.clause_break {
            return false;
        }
        let word = token.text.as_str();
        if matches_idiom(&later[index..]) {
            return true;
        }
        if RETRACTION_VERBS.contains(&word) {
            return !verb_is_negated_or_infinitive(&later[..index]);
        }
        let rest_of_clause = later[index + 1..]
            .iter()
            .take_while(|next| !next.clause_break)
            .filter(|next| !next.quoted)
            .map(|next| next.text.as_str());
        if RETRACTION_PAIRS.iter().any(|(first_words, second_words)| {
            first_words.contains(&word)
                && rest_of_clause
                    .clone()
                    .any(|next| second_words.contains(&next))
        }) {
            return true;
        }
        if !is_retracting_negation(word) {
            return false;
        }
        let same_clause: Vec<&IntentToken> = later[index + 1..]
            .iter()
            .copied()
            .take_while(|next| !next.clause_break)
            .collect();
        let governed = same_clause.iter().position(|next| {
            let text = next.text.as_str();
            !RETRACTION_FILLERS.contains(&text) && !ENDED_WORDS.contains(&text)
        });
        governed.is_none_or(|position| {
            let next = same_clause[position];
            let text = next.text.as_str();
            !next.quoted
                && (RETRACTION_TARGETS.contains(&text)
                    || NEEDLESS_WORDS.contains(&text)
                    || (DESIRE_WORDS.contains(&text)
                        && (wish_has_ended(&same_clause)
                            || desire_governs_reminder(&same_clause[position + 1..]))))
        })
    })
}

/// Whether the words after a desire verb name the reminder as its object: the first word that is
/// not an [`OBJECT_LEAD_INS`] entry is a [`REMINDER_OBJECT_WORDS`] entry ("that reminder", "the
/// reminder", "to be reminded", "you to remind me"). "to miss it" names something else, and a
/// quoted word is reported speech rather than the user's object.
fn desire_governs_reminder(rest_of_clause: &[&IntentToken]) -> bool {
    rest_of_clause
        .iter()
        .find(|token| !OBJECT_LEAD_INS.contains(&token.text.as_str()))
        .is_some_and(|object| {
            !object.quoted && REMINDER_OBJECT_WORDS.contains(&object.text.as_str())
        })
}

/// Whether a clause after a retracting negation says the wish or need has ended: "no longer",
/// "anymore", or the two words "any more".
fn wish_has_ended(clause: &[&IntentToken]) -> bool {
    clause.iter().enumerate().any(|(index, token)| {
        let text = token.text.as_str();
        !token.quoted
            && (ENDED_WORDS.contains(&text)
                || (text == "any"
                    && clause
                        .get(index + 1)
                        .is_some_and(|next| !next.quoted && next.text == "more")))
    })
}

fn matches_idiom(window: &[&IntentToken]) -> bool {
    RETRACTION_IDIOMS.iter().any(|idiom| {
        window.get(..idiom.len()).is_some_and(|candidate| {
            candidate.iter().zip(idiom.iter()).all(|(token, expected)| {
                !token.quoted && !token.clause_break && token.text == *expected
            })
        })
    })
}

/// Whether the word right before a retraction verb (skipping fillers such as "actually" or
/// "just", within the same clause) negates it or makes it an infinitive, so the verb is not a
/// withdrawal: "don't forget the ladder", "never cancel", "to cancel the subscription".
fn verb_is_negated_or_infinitive(before: &[&IntentToken]) -> bool {
    before
        .iter()
        .rev()
        .take_while(|previous| !previous.clause_break)
        .map(|previous| previous.text.as_str())
        .find(|previous| !RETRACTION_FILLERS.contains(previous))
        .is_some_and(|previous| previous == "to" || is_retracting_negation(previous))
}

/// Whether a reminder request whose cue ends at `cue_end` governs the quoted time `span`: the
/// span follows the cue and no token between them breaks the clause or disqualifies the time
/// (negation, exclusion, recall sense, completed work or a past-tense event).
fn request_governs_span(tokens: &[IntentToken], cue_end: usize, span: SourceSpan) -> bool {
    span.start >= cue_end
        && !tokens
            .iter()
            .any(|token| token.start >= cue_end && token.start < span.start && detaches_time(token))
}

fn detaches_time(token: &IntentToken) -> bool {
    let word = token.text.as_str();
    token.clause_break
        || is_negating_word(word)
        || EXCLUDING_WORDS.contains(&word)
        || RECALL_WORDS.contains(&word)
        || COMPLETED_WORDS.contains(&word)
        || PAST_EVENT_WORDS.contains(&word)
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
        .any(|word| is_negating_word(word) || COMPLETED_WORDS.contains(word))
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
