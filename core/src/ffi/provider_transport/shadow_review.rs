//! Shadow review over the core handle (E03b).
//!
//! Native code runs the sampled, diagnostic-only review cases that `crate::review::shadow`
//! reserves, through the provider transport it already registered (V05b). As there, a caller
//! names only a case; the pinned review profile, destinations, credential reference and request
//! text are derived inside the core from stored state, and every attempt is re-authorized by
//! `authorize_shadow_dispatch` immediately before the request is handed to native code.
//!
//! * `ohand_core_start_shadow_selection` offers one case for sampling (`select_for_shadow_review`)
//!   and answers with one event: `{"operation_id","selection":"selected"|"already_selected"|
//!   "skipped","skip":code|null,"denial":code|null,"record":{...}|null}`. Nothing is sent.
//! * `ohand_core_start_shadow_review` leases one selected case and runs it. A refused case is
//!   recorded as unreviewed and answered with one `{"phase":"completed","denial":code,"record"}`
//!   event. An authorized case answers `{"phase":"dispatched"}` first, then `{"phase":
//!   "completed","denial":null,"record":{...}}` once the outcome is stored. The review is a single
//!   provider request: a disagreement is stored as fixed diagnostic codes and never triggers a
//!   second round. Cancel it with `ohand_core_cancel_provider_exchange`; answer its native sends
//!   with the provider exchange answers.
//! * `ohand_core_start_shadow_record` reads one stored case.
//!
//! A verdict is diagnostic only. It is written to the case's own job row and nowhere else: this
//! module never touches item, reminder, proposal or permission state, and the content it stores
//! and reports is limited to fixed codes (never provider text, capture text, routes or secrets).

use super::*;
use crate::interpretation::contracts::{Proposal, ReminderProposal};
use crate::interpretation::instructions::InterpretationMapping;
use crate::jobs::queue::{
    complete_job_in_tx, JobStatus, PROFILE_MISSING_REASON, PROFILE_REVOKED_REASON,
};
use crate::privacy::routing::JOB_TYPE_SHADOW_REVIEW;
use crate::providers::contracts::{FailureKind, ProviderFailure};
use crate::review::shadow::{
    authorize_shadow_dispatch, load_shadow_record, record_shadow_failure, record_shadow_unreviewed,
    select_for_shadow_review, ShadowCandidate, ShadowDispatchDecision, ShadowDispatchDenial,
    ShadowOutcome, ShadowPolicy, ShadowRecord, ShadowSelection, ShadowSkip, UnreviewedReason,
};
use crate::store::schema::Database;
use rusqlite::Transaction;
use uuid::Uuid;

/// Prefix of the fixed verdict code stored on a case that completed: `shadow_verdict:agreement`
/// or `shadow_verdict:disagreement:<difference>[,<difference>...]`.
const VERDICT_PREFIX: &str = "shadow_verdict:";
const JOB_SCHEMA_VERSION: i32 = 1;
const LEASE_SECONDS: i64 = 600;

mod shadow_failures {
    use super::failures::fixed;
    use super::{AbiFailure, ErrorClass};

    pub(super) const NOT_SHADOW_JOB: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "not_shadow_job",
        "the job is not a shadow review case",
    );
    pub(super) const NOT_LEASABLE: AbiFailure = fixed(
        ErrorClass::Permanent,
        "not_leasable",
        "the shadow review case is finished, leased or waiting for a retry",
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyRequest {
    enabled: bool,
    sample_per_mille: u16,
    window_seconds: i64,
    max_requests_per_window: u32,
    max_attempts_per_sample: u32,
}

impl PolicyRequest {
    fn policy(&self) -> ShadowPolicy {
        ShadowPolicy {
            enabled: self.enabled,
            sample_per_mille: self.sample_per_mille,
            window_seconds: self.window_seconds,
            max_requests_per_window: self.max_requests_per_window,
            max_attempts_per_sample: self.max_attempts_per_sample,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRequest {
    item_id: String,
    source_revision: i32,
    request_version: String,
    review_profile_version: String,
    policy: PolicyRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {
    job_id: String,
    policy: PolicyRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordRequest {
    job_id: String,
}

#[derive(Serialize)]
struct RecordJson {
    job_id: String,
    item_id: String,
    source_revision: i32,
    request_version: String,
    review_profile_version: String,
    attempts_used: i32,
    outcome: &'static str,
    reason: Option<String>,
    last_failure: Option<String>,
    verdict: Option<&'static str>,
    differences: Vec<String>,
}

#[derive(Serialize)]
struct SelectionEvent {
    operation_id: u64,
    selection: &'static str,
    skip: Option<&'static str>,
    denial: Option<String>,
    record: Option<RecordJson>,
}

#[derive(Serialize)]
struct ReviewCompleted {
    operation_id: u64,
    phase: &'static str,
    denial: Option<String>,
    record: RecordJson,
}

#[derive(Serialize)]
struct RecordEvent {
    operation_id: u64,
    record: RecordJson,
}

fn parse_request<T: serde::de::DeserializeOwned>(
    request: *const u8,
    request_len: usize,
) -> Result<T, AbiFailure> {
    if request_len > OHAND_CORE_MAX_PROVIDER_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if request_len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if request.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    // SAFETY: the exports document that `request` points at `request_len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(request, request_len) };
    serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)
}

fn encode<T: Serialize>(event: &T) -> Result<Vec<u8>, AbiFailure> {
    serde_json::to_vec(event).map_err(|_| AbiFailure::INTERNAL)
}

fn unreviewed_code(reason: UnreviewedReason) -> &'static str {
    match reason {
        UnreviewedReason::Timeout => "timeout",
        UnreviewedReason::Cancelled => "cancelled",
        UnreviewedReason::DispatchDenied => "dispatch_denied",
        UnreviewedReason::Retired => "retired",
    }
}

fn denial_code(denial: ShadowDispatchDenial) -> String {
    match denial {
        ShadowDispatchDenial::Disabled => "disabled".to_string(),
        ShadowDispatchDenial::InvalidPolicy => "invalid_policy".to_string(),
        ShadowDispatchDenial::JobNotFound => "job_not_found".to_string(),
        ShadowDispatchDenial::NotAShadowJob => "not_shadow_job".to_string(),
        ShadowDispatchDenial::NotLeased => "not_leased".to_string(),
        ShadowDispatchDenial::AttemptsExhausted => "attempts_exhausted".to_string(),
        ShadowDispatchDenial::AlreadyDispatched => "already_dispatched".to_string(),
        ShadowDispatchDenial::BudgetExhausted { .. } => "budget_exhausted".to_string(),
        ShadowDispatchDenial::ItemUnavailable => "item_unavailable".to_string(),
        ShadowDispatchDenial::NoRemoteProfile => "no_remote_profile".to_string(),
        ShadowDispatchDenial::NotAuthorized(reason) => denial_failure(reason).code.to_string(),
    }
}

fn skip_codes(skip: ShadowSkip) -> (&'static str, Option<String>) {
    match skip {
        ShadowSkip::Disabled => ("disabled", None),
        ShadowSkip::InvalidPolicy => ("invalid_policy", None),
        ShadowSkip::ItemUnavailable => ("item_unavailable", None),
        ShadowSkip::NotSampled => ("not_sampled", None),
        ShadowSkip::BudgetExhausted { .. } => ("budget_exhausted", None),
        ShadowSkip::NotAuthorized(reason) => (
            "not_authorized",
            Some(denial_failure(reason).code.to_string()),
        ),
    }
}

/// The verdict code stored on a completed case, split into verdict and differences.
fn parse_verdict(failure_reason: Option<&str>) -> (Option<&'static str>, Vec<String>) {
    let Some(code) = failure_reason.and_then(|reason| reason.strip_prefix(VERDICT_PREFIX)) else {
        return (None, Vec::new());
    };
    if code == "agreement" {
        return (Some("agreement"), Vec::new());
    }
    match code.strip_prefix("disagreement:") {
        Some(differences) => (
            Some("disagreement"),
            differences.split(',').map(str::to_string).collect(),
        ),
        None => (None, Vec::new()),
    }
}

fn record_json(connection: &Connection, record: ShadowRecord) -> Result<RecordJson, AbiFailure> {
    let (outcome, reason) = match &record.outcome {
        ShadowOutcome::Pending => ("pending", None),
        ShadowOutcome::Reviewed => ("reviewed", None),
        ShadowOutcome::Unreviewed(reason) => ("unreviewed", Some(unreviewed_code(*reason).into())),
        ShadowOutcome::Error(code) => ("error", Some(code.clone())),
    };
    let (verdict, differences) = if record.outcome == ShadowOutcome::Reviewed {
        let failure_reason: Option<String> = connection
            .query_row(
                "SELECT failure_reason FROM jobs WHERE job_id = ?",
                [&record.job_id],
                |row| row.get(0),
            )
            .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
        parse_verdict(failure_reason.as_deref())
    } else {
        (None, Vec::new())
    };
    Ok(RecordJson {
        job_id: record.job_id,
        item_id: record.item_id,
        source_revision: record.source_revision,
        request_version: record.request_version,
        review_profile_version: record.review_profile_version,
        attempts_used: record.attempts_used,
        outcome,
        reason,
        last_failure: record.last_failure,
        verdict,
        differences,
    })
}

fn load_record_json(connection: &Connection, job_id: &str) -> Result<RecordJson, AbiFailure> {
    let record = load_shadow_record(connection, job_id)
        .map_err(storage_failure)?
        .ok_or(AbiFailure::NOT_FOUND)?;
    record_json(connection, record)
}

/// The facets of the stored interpretation result a review is compared against.
struct PrimaryFacets {
    item_type: Option<String>,
    reminder: Option<ReminderProposal>,
    session_topic: Option<String>,
    abstained: bool,
}

fn load_primary(
    connection: &Connection,
    item_id: &str,
    source_revision: i32,
    request_version: &str,
) -> Result<Option<PrimaryFacets>, AbiFailure> {
    type Row = (Option<String>, Option<String>, Option<String>, bool);
    let row: Option<Row> = connection
        .query_row(
            "SELECT proposal_type, reminder_proposal, session_topic_proposal, abstained \
             FROM proposals WHERE item_id = ? AND source_revision = ? AND request_version = ? \
             ORDER BY CASE applied_state WHEN 'applied' THEN 0 ELSE 1 END, created_at LIMIT 1",
            rusqlite::params![item_id, source_revision, request_version],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
    row.map(|(item_type, reminder_json, session_topic, abstained)| {
        let reminder = reminder_json
            .map(|json| serde_json::from_str::<ReminderProposal>(&json))
            .transpose()
            .map_err(|_| failures::INVALID_JOB)?;
        Ok(PrimaryFacets {
            item_type,
            reminder,
            session_topic,
            abstained,
        })
    })
    .transpose()
}

/// Fixed names of the facets on which the reviewer's proposal differs from the stored one.
fn differences(primary: &PrimaryFacets, review: &Proposal) -> Vec<&'static str> {
    let mut differing = Vec::new();
    if primary.item_type.as_deref() != review.item_type.map(|item_type| item_type.as_str()) {
        differing.push("item_type");
    }
    match (&primary.reminder, &review.reminder_proposal) {
        (None, None) => {}
        (Some(stored), Some(reviewed)) => {
            if stored.instant != reviewed.instant || stored.quality != reviewed.quality {
                differing.push("reminder_time");
            }
        }
        _ => differing.push("reminder_presence"),
    }
    let reviewed_topic = review
        .session_topic_proposal
        .as_ref()
        .map(|topic| topic.topic.trim());
    if primary.session_topic.as_deref().map(str::trim) != reviewed_topic {
        differing.push("session_topic");
    }
    if primary.abstained != review.abstention.is_some() {
        differing.push("abstention");
    }
    differing
}

enum Lease {
    Leased(i32),
    /// The case was retired (or found unsupported) instead of leased; its terminal state is stored.
    Settled(&'static str),
}

fn parse_stored_instant(text: &str) -> Result<DateTime<Utc>, AbiFailure> {
    DateTime::parse_from_rfc3339(text)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| failures::INVALID_JOB)
}

fn retire(
    transaction: &Transaction<'_>,
    job_id: &str,
    status: JobStatus,
    reason: &str,
) -> Result<(), AbiFailure> {
    transaction
        .execute(
            "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
             WHERE job_id = ? AND status IN (?, ?)",
            rusqlite::params![
                status.as_str(),
                reason,
                job_id,
                JobStatus::Queued.as_str(),
                JobStatus::Running.as_str()
            ],
        )
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
    Ok(())
}

/// Leases exactly this case, with the rules the queue's claim applies to any job: only a ready
/// queued case or one whose lease expired is leased, each lease is a further attempt, and a case
/// that can never run (unsupported schema, revoked or missing profile, deleted or revised item)
/// is retired durably instead.
fn lease_shadow_job(
    database: &mut Database,
    job_id: &str,
    now: DateTime<Utc>,
) -> Result<Lease, AbiFailure> {
    let transaction = database.immediate_transaction().map_err(storage_failure)?;
    type Row = (
        String,
        String,
        i32,
        Option<String>,
        Option<String>,
        i32,
        Option<String>,
        String,
        i32,
    );
    let row: Row = transaction
        .query_row(
            "SELECT job_type, status, attempt_count, next_attempt_at, lease_expires_at, \
             job_schema_version, profile_version, item_id, source_revision FROM jobs \
             WHERE job_id = ?",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .optional()
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?
        .ok_or(AbiFailure::NOT_FOUND)?;
    let (
        job_type,
        status,
        attempt_count,
        next_attempt_at,
        lease_expires_at,
        schema_version,
        profile_version,
        item_id,
        source_revision,
    ) = row;
    if job_type != JOB_TYPE_SHADOW_REVIEW {
        return Err(shadow_failures::NOT_SHADOW_JOB);
    }
    let ready = if status == JobStatus::Queued.as_str() {
        match next_attempt_at {
            Some(text) => parse_stored_instant(&text)? <= now,
            None => true,
        }
    } else if status == JobStatus::Running.as_str() {
        match lease_expires_at {
            Some(text) => parse_stored_instant(&text)? < now,
            None => true,
        }
    } else {
        false
    };
    if !ready {
        return Err(shadow_failures::NOT_LEASABLE);
    }

    let settled = |transaction: Transaction<'_>, code| -> Result<Lease, AbiFailure> {
        transaction
            .commit()
            .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
        Ok(Lease::Settled(code))
    };
    if schema_version != JOB_SCHEMA_VERSION {
        retire(
            &transaction,
            job_id,
            JobStatus::Failed,
            "unsupported_job_version",
        )?;
        return settled(transaction, "unsupported_job_version");
    }
    if let Some(profile_version) = profile_version {
        let revoked_at: Option<Option<String>> = transaction
            .query_row(
                "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
                [&profile_version],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
        let reason = match revoked_at {
            None => Some(PROFILE_MISSING_REASON),
            Some(Some(_)) => Some(PROFILE_REVOKED_REASON),
            Some(None) => None,
        };
        if let Some(reason) = reason {
            retire(&transaction, job_id, JobStatus::Cancelled, reason)?;
            return settled(transaction, "retired");
        }
    }
    let item: Option<(bool, i32)> = transaction
        .query_row(
            "SELECT lifecycle_state != 'deleted', revision FROM items WHERE item_id = ?",
            [&item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
    let cancel_reason = match item {
        None => Some("item_not_found"),
        Some((false, _)) => Some("item_deleted"),
        Some((true, revision)) if revision != source_revision => Some("stale_revision"),
        Some((true, _)) => None,
    };
    if let Some(reason) = cancel_reason {
        retire(&transaction, job_id, JobStatus::Cancelled, reason)?;
        return settled(transaction, "retired");
    }

    let lease_attempt = attempt_count + 1;
    let lease_expires = now + chrono::Duration::seconds(LEASE_SECONDS);
    let leased = transaction
        .execute(
            "UPDATE jobs SET status = ?, lease_expires_at = ?, attempt_count = ? \
             WHERE job_id = ? AND status = ? AND attempt_count = ?",
            rusqlite::params![
                JobStatus::Running.as_str(),
                lease_expires.to_rfc3339(),
                lease_attempt,
                job_id,
                status,
                attempt_count
            ],
        )
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
    if leased != 1 {
        return Err(shadow_failures::NOT_LEASABLE);
    }
    transaction
        .commit()
        .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
    Ok(Lease::Leased(lease_attempt))
}

/// Builds the review request from stored state: the reviewer sees the text the interpretation
/// saw (the item's latest user correction, otherwise the capture text) under the case's pinned
/// review profile, to destinations the route's `review` grant covers.
fn prepare_review(connection: &Connection, job_id: &str) -> Result<Prepared, AbiFailure> {
    let target = match resolve_execution_target(connection, job_id).map_err(storage_failure)? {
        ExecutionResolution::Denied(reason) => return Err(denial_failure(reason)),
        ExecutionResolution::Local => return Err(failures::NOT_REMOTE),
        ExecutionResolution::Remote(target) => target,
    };
    let row = load_job_row(connection, job_id)?;
    if row.job_type != JOB_TYPE_SHADOW_REVIEW {
        return Err(shadow_failures::NOT_SHADOW_JOB);
    }
    let profile = load_profile(connection, &target.profile_version)?;
    if profile.protocol() == ProviderProtocol::SelfHosted {
        return Err(failures::UNSUPPORTED_PROTOCOL);
    }
    if target.credential_ref.as_deref() != Some(profile.credential_ref().as_str()) {
        return Err(failures::MALFORMED_PROFILE);
    }
    if row.item_revision != row.source_revision {
        return Err(failures::STALE_SOURCE);
    }
    let source_revision = u64::try_from(row.source_revision).map_err(|_| failures::INVALID_JOB)?;
    let (text, text_basis) = match latest_text_correction(connection, job_id)? {
        Some((correction_record_id, corrected_text)) => (
            corrected_text,
            TextBasis::Correction {
                correction_record_id,
                item_revision: source_revision,
            },
        ),
        None => (
            row.text.ok_or(failures::NO_TEXT)?,
            TextBasis::Original {
                item_revision: source_revision,
            },
        ),
    };
    if text.trim().is_empty() {
        return Err(failures::NO_TEXT);
    }
    let reference_time = DateTime::parse_from_rfc3339(&row.capture_instant)
        .map_err(|_| failures::INVALID_JOB)?
        .with_timezone(&Utc);
    let request = InterpretationRequest::new(
        row.capture_id,
        source_revision,
        text_basis,
        text,
        row.request_version.ok_or(failures::INVALID_JOB)?,
        M1_INSTRUCTION_VERSION,
        &profile,
        row.route_id,
        TimeContext {
            timezone: row.timezone_id,
            locale: row.locale,
            reference_time,
            utc_offset_at_capture: row.utc_offset_minutes.saturating_mul(60),
            calendar: row.calendar,
        },
    )
    .map_err(|_| failures::INVALID_JOB)?;
    Ok(Prepared {
        profile,
        request,
        destinations: target.destinations,
    })
}

struct Dispatch {
    prepared: Prepared,
    primary: PrimaryFacets,
    item_id: String,
    lease_attempt: i32,
}

enum Begun {
    Finished {
        denial: String,
        record: Box<RecordJson>,
    },
    Dispatch(Box<Dispatch>),
}

/// Leases the case and runs every check that must pass before a request may leave the device.
/// A refusal ends the case as unreviewed with no request sent.
fn begin_review(
    database: &mut Database,
    job_id: &str,
    policy: &ShadowPolicy,
    now: DateTime<Utc>,
) -> Result<Begun, AbiFailure> {
    let lease_attempt = match lease_shadow_job(database, job_id, now)? {
        Lease::Leased(attempt) => attempt,
        Lease::Settled(code) => {
            return Ok(Begun::Finished {
                denial: code.to_string(),
                record: Box::new(load_record_json(database.conn(), job_id)?),
            })
        }
    };
    let refuse = |database: &mut Database, code: String| -> Result<Begun, AbiFailure> {
        record_shadow_unreviewed(
            database,
            job_id,
            lease_attempt,
            UnreviewedReason::DispatchDenied,
        )
        .map_err(storage_failure)?;
        Ok(Begun::Finished {
            denial: code,
            record: Box::new(load_record_json(database.conn(), job_id)?),
        })
    };

    let case = load_shadow_record(database.conn(), job_id)
        .map_err(storage_failure)?
        .ok_or(AbiFailure::NOT_FOUND)?;
    let Some(primary) = load_primary(
        database.conn(),
        &case.item_id,
        case.source_revision,
        &case.request_version,
    )?
    else {
        return refuse(database, "primary_missing".to_string());
    };
    match authorize_shadow_dispatch(database, job_id, lease_attempt, policy, now)
        .map_err(storage_failure)?
    {
        ShadowDispatchDecision::Denied(denial) => return refuse(database, denial_code(denial)),
        ShadowDispatchDecision::Allowed(_) => {}
    }
    match prepare_review(database.conn(), job_id) {
        Ok(prepared) => Ok(Begun::Dispatch(Box::new(Dispatch {
            prepared,
            primary,
            item_id: case.item_id,
            lease_attempt,
        }))),
        Err(failure) => refuse(database, failure.code.to_string()),
    }
}

#[derive(Clone)]
enum ReviewResult {
    Verdict(Vec<&'static str>),
    Failure(ProviderFailure),
}

fn failure_of(failure: &AbiFailure) -> ProviderFailure {
    let kind =
        serde_json::from_value::<FailureKind>(serde_json::Value::String(failure.code.to_string()))
            .unwrap_or(FailureKind::Rejected);
    ProviderFailure::new(kind)
}

/// Turns the exchange's result into a verdict by mapping the reviewer's reply to a proposal the
/// same way an interpretation reply is mapped, then comparing it with the stored result.
fn review_result(
    outcome: Result<Vec<u8>, AbiFailure>,
    request: &InterpretationRequest,
    item_id: &str,
    primary: &PrimaryFacets,
) -> ReviewResult {
    let invalid = || ReviewResult::Failure(ProviderFailure::new(FailureKind::InvalidOutput));
    let bytes = match outcome {
        Ok(bytes) => bytes,
        Err(failure) => return ReviewResult::Failure(failure_of(&failure)),
    };
    let Some(output) = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("output").cloned())
        .and_then(|output| serde_json::from_value::<InterpretationOutput>(output).ok())
    else {
        return invalid();
    };
    let Ok(mapping) = InterpretationMapping::new(request, item_id, Uuid::new_v4().to_string())
    else {
        return invalid();
    };
    match mapping.map_output(&output) {
        Ok(proposal) => ReviewResult::Verdict(differences(primary, &proposal)),
        Err(_) => invalid(),
    }
}

/// Completes the leased case with its verdict code. If the case was retired meanwhile (profile
/// revoked), the retirement stands and the case reads as unreviewed.
fn record_verdict(
    database: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    differing: &[&'static str],
) -> Result<(), AbiFailure> {
    let code = if differing.is_empty() {
        format!("{VERDICT_PREFIX}agreement")
    } else {
        format!("{VERDICT_PREFIX}disagreement:{}", differing.join(","))
    };
    let transaction = database.immediate_transaction().map_err(storage_failure)?;
    match complete_job_in_tx(&transaction, job_id, lease_attempt) {
        Ok(()) => {
            transaction
                .execute(
                    "UPDATE jobs SET failure_reason = ? WHERE job_id = ? AND job_type = ? \
                     AND status = ? AND attempt_count = ?",
                    rusqlite::params![
                        code,
                        job_id,
                        JOB_TYPE_SHADOW_REVIEW,
                        JobStatus::Completed.as_str(),
                        lease_attempt
                    ],
                )
                .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
            transaction
                .commit()
                .map_err(|error| storage_failure(anyhow::Error::from(error)))
        }
        Err(_) => {
            let retired: bool = transaction
                .query_row(
                    "SELECT status = ? FROM jobs WHERE job_id = ?",
                    rusqlite::params![JobStatus::Cancelled.as_str(), job_id],
                    |row| row.get(0),
                )
                .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
            if retired {
                return transaction
                    .commit()
                    .map_err(|error| storage_failure(anyhow::Error::from(error)));
            }
            drop(transaction);
            record_shadow_unreviewed(database, job_id, lease_attempt, UnreviewedReason::Retired)
                .map(|_| ())
                .map_err(storage_failure)
        }
    }
}

fn finish_review(
    database: &mut Database,
    operation_id: u64,
    job_id: &str,
    lease_attempt: i32,
    policy: &ShadowPolicy,
    result: &ReviewResult,
) -> Result<Vec<u8>, AbiFailure> {
    match result {
        ReviewResult::Failure(failure) => {
            record_shadow_failure(database, job_id, lease_attempt, failure, policy, Utc::now())
                .map_err(storage_failure)?;
        }
        ReviewResult::Verdict(differing) => {
            record_verdict(database, job_id, lease_attempt, differing)?;
        }
    }
    encode(&ReviewCompleted {
        operation_id,
        phase: "completed",
        denial: None,
        record: load_record_json(database.conn(), job_id)?,
    })
}

struct FinalReview {
    operation_id: u64,
    job_id: String,
    lease_attempt: i32,
    policy: ShadowPolicy,
    result: ReviewResult,
}

/// Queues the final event for the case. Like the interpretation exchange's final event it follows
/// the `dispatched` event; if the core is gone, the case keeps its lease until it expires.
fn deliver_review_final(handle: u64, review: Arc<FinalReview>) {
    for _ in 0..QUEUE_RETRY_LIMIT {
        let Ok(core) = instance::lookup(handle) else {
            return;
        };
        let attempt = Arc::clone(&review);
        let job: JobFn = Box::new(move |database| {
            let mut database = lock(database);
            finish_review(
                &mut database,
                attempt.operation_id,
                &attempt.job_id,
                attempt.lease_attempt,
                &attempt.policy,
                &attempt.result,
            )
        });
        match core.submit(review.operation_id, job) {
            Err(failure) if failure == AbiFailure::QUEUE_FULL => {
                std::thread::sleep(QUEUE_RETRY_PAUSE);
            }
            _ => return,
        }
    }
}

fn start_review_job(
    handle: u64,
    operation_id: u64,
    request: ReviewRequest,
    registration: Arc<Registration>,
    slot: SlotGuard,
    receiver: Receiver<Completion>,
    cancel: CancelToken,
) -> JobFn {
    Box::new(move |database| {
        let policy = request.policy.policy();
        let begun = {
            let mut database = lock(database);
            begin_review(&mut database, &request.job_id, &policy, Utc::now())?
        };
        let dispatch = match begun {
            Begun::Finished { denial, record } => {
                return encode(&ReviewCompleted {
                    operation_id,
                    phase: "completed",
                    denial: Some(denial),
                    record: *record,
                })
            }
            Begun::Dispatch(dispatch) => *dispatch,
        };
        let end = Arc::new(ExchangeEnd {
            handle,
            operation_id,
            registration,
            receiver: Mutex::new(receiver),
            authorized_origins: dispatch.prepared.destinations.clone(),
            capability: CAPABILITY_REVIEW,
        });
        let job_id = request.job_id;
        std::thread::Builder::new()
            .name("ohand-shadow-review".to_string())
            .spawn(move || {
                let _slot = slot;
                let Dispatch {
                    prepared,
                    primary,
                    item_id,
                    lease_attempt,
                } = dispatch;
                let outcome =
                    catch_unwind(AssertUnwindSafe(|| run_exchange(&prepared, end, &cancel)))
                        .unwrap_or(Err(AbiFailure::INTERNAL));
                let result = review_result(outcome, &prepared.request, &item_id, &primary);
                deliver_review_final(
                    handle,
                    Arc::new(FinalReview {
                        operation_id,
                        job_id,
                        lease_attempt,
                        policy,
                        result,
                    }),
                );
            })
            .map_err(|_| AbiFailure::INTERNAL)?;
        encode(&Dispatched {
            operation_id,
            phase: "dispatched",
        })
    })
}

fn select_case(
    database: &mut Database,
    operation_id: u64,
    request: &SelectionRequest,
) -> Result<Vec<u8>, AbiFailure> {
    let policy = request.policy.policy();
    let skipped = |skip: &'static str| SelectionEvent {
        operation_id,
        selection: "skipped",
        skip: Some(skip),
        denial: None,
        record: None,
    };
    if policy.enabled
        && load_primary(
            database.conn(),
            &request.item_id,
            request.source_revision,
            &request.request_version,
        )?
        .is_none()
    {
        return encode(&skipped("primary_missing"));
    }
    let candidate = ShadowCandidate {
        item_id: &request.item_id,
        source_revision: request.source_revision,
        request_version: &request.request_version,
        review_profile_version: &request.review_profile_version,
    };
    let selection = select_for_shadow_review(database, &candidate, &policy, Utc::now())
        .map_err(storage_failure)?;
    let event = match selection {
        ShadowSelection::Selected(record) => SelectionEvent {
            operation_id,
            selection: "selected",
            skip: None,
            denial: None,
            record: Some(record_json(database.conn(), record)?),
        },
        ShadowSelection::AlreadySelected(record) => SelectionEvent {
            operation_id,
            selection: "already_selected",
            skip: None,
            denial: None,
            record: Some(record_json(database.conn(), record)?),
        },
        ShadowSelection::Skipped(skip) => {
            let (code, denial) = skip_codes(skip);
            SelectionEvent {
                denial,
                ..skipped(code)
            }
        }
    };
    encode(&event)
}

/// Queues the offer of one case for shadow sampling, from the JSON `{"item_id","source_revision",
/// "request_version","review_profile_version","policy":{"enabled","sample_per_mille",
/// "window_seconds","max_requests_per_window","max_attempts_per_sample"}}` at `request`. One
/// event answers it; nothing is sent to a provider. See the module documentation.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_shadow_selection(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let selection: SelectionRequest = parse_request(request, request_len)?;
        core.submit(
            operation_id,
            Box::new(move |database| {
                let mut database = lock(database);
                select_case(&mut database, operation_id, &selection)
            }),
        )
    })
}

/// Queues the review of the selected case in the JSON `{"job_id","policy":{...}}` at `request`
/// through the registered provider transport. See the module documentation for the events.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_shadow_review(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let review: ReviewRequest = parse_request(request, request_len)?;
        let registration = lock(&TRANSPORTS)
            .get(&handle)
            .cloned()
            .ok_or(failures::TRANSPORT_NOT_REGISTERED)?;

        let (sender, receiver) = channel();
        let cancel = CancelToken::new();
        let key = (handle, operation_id);
        {
            let mut exchanges = lock(&EXCHANGES);
            if exchanges.contains_key(&key) {
                return Err(failures::DUPLICATE_OPERATION);
            }
            exchanges.insert(
                key,
                Pending {
                    sender,
                    cancel: cancel.clone(),
                },
            );
        }
        let slot = SlotGuard { key };
        let job = start_review_job(
            handle,
            operation_id,
            review,
            registration,
            slot,
            receiver,
            cancel,
        );
        core.submit(operation_id, job)
    })
}

/// Queues a read of the stored case named in the JSON `{"job_id"}` at `request`. The event is
/// `{"operation_id","record":{...}}`, or `not_found` for an unknown or non-shadow job.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_shadow_record(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let read: RecordRequest = parse_request(request, request_len)?;
        core.submit(
            operation_id,
            Box::new(move |database| {
                let database = lock(database);
                encode(&RecordEvent {
                    operation_id,
                    record: load_record_json(database.conn(), &read.job_id)?,
                })
            }),
        )
    })
}

#[cfg(test)]
mod tests;
