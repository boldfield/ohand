//! Queueing of on-device `transcription_attachment` jobs for voice captures (C07).
//!
//! The native transcription handler (C05b) only runs jobs that already exist, and
//! [`apply_transcription_outcome`](super::apply_transcription_outcome) settles them. This module
//! is the one place that creates them:
//!
//! * [`enqueue_for_new_item`] runs inside the import transaction of an audio-only capture, so the
//!   item and its job commit together and an import retry (which finds the item) queues nothing;
//! * [`backfill_missing_jobs`] runs at the start of every drain and gives each voice capture that
//!   has no queued, running or completed job one, which covers captures stored before imports
//!   queued their own job. It works in bounded batches, one transaction per batch;
//! * [`requeue_transcription`] is the explicit retry of one item whose earlier attempt ended as
//!   unsupported or failed.
//!
//! Jobs are never pinned to a provider profile: transcription is on device only and
//! `authorize_job` treats an unpinned transcription job as local, and they record no request
//! version either (the job contract marks both as not applicable). The generic queue dedupes on
//! item, type, revision, profile and request version, which would make a retry at the same revision
//! collide with the attempt it replaces, so these jobs are inserted here and deduplicated only
//! against jobs that are still queued or running.

use super::INTERPRETATION_JOB_SCHEMA_VERSION;
use crate::domain::items::TransitionValidity;
use crate::domain::items::{load_item_state, validate_state_transition, StateTransition};
use crate::domain::status::TranscriptionState;
use crate::jobs::queue::{Job, JobStatus};
use crate::privacy::routing::JOB_TYPE_TRANSCRIPTION_ATTACHMENT;
use crate::store::schema::Database;
use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;
use uuid::Uuid;

/// Most voice captures one backfill transaction gives a job.
pub const BACKFILL_BATCH_SIZE: usize = 50;

/// Statuses of a transcription job that mean the work is still wanted or already done.
const LIVE_OR_DONE_STATUSES: &str = "'queued', 'running', 'completed'";

/// Queue the transcription job of an audio-only item created in `transaction`. Fails if a queued
/// or running transcription job for the item already exists.
pub fn enqueue_for_new_item(
    transaction: &Transaction<'_>,
    item_id: &str,
    source_revision: i32,
    now: DateTime<Utc>,
) -> anyhow::Result<Job> {
    let in_flight: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE item_id = ? AND job_type = ?
                         AND status IN ('queued', 'running'))",
        rusqlite::params![item_id, JOB_TYPE_TRANSCRIPTION_ATTACHMENT],
        |row| row.get(0),
    )?;
    if in_flight {
        anyhow::bail!("a transcription job for item {item_id} is already queued or running");
    }
    let job_id = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO jobs (
            job_id, job_schema_version, item_id, job_type, source_revision,
            profile_version, request_version, status, attempt_count, created_at
        ) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, 0, ?)",
        rusqlite::params![
            job_id,
            INTERPRETATION_JOB_SCHEMA_VERSION,
            item_id,
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
            source_revision,
            JobStatus::Queued.as_str(),
            now.to_rfc3339(),
        ],
    )?;
    Ok(Job {
        job_id,
        job_schema_version: INTERPRETATION_JOB_SCHEMA_VERSION,
        item_id: item_id.to_string(),
        job_type: JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
        source_revision,
        profile_version: None,
        request_version: None,
        status: JobStatus::Queued,
        failure_reason: None,
        attempt_count: 0,
        next_attempt_at: None,
        lease_expires_at: None,
        created_at: now,
    })
}

/// Give every non-deleted voice capture that is waiting for transcription, still has its audio,
/// has no text and has no queued, running or completed transcription job one such job. Returns how
/// many jobs were queued. Running it again immediately queues nothing.
pub fn backfill_missing_jobs(database: &mut Database, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let mut queued = 0;
    let mut after_item_id = String::new();
    loop {
        let transaction = database.immediate_transaction()?;
        let candidates = select_candidates(&transaction, &after_item_id)?;
        let Some(last) = candidates.last() else {
            return Ok(queued);
        };
        after_item_id = last.0.clone();
        let batch_was_full = candidates.len() == BACKFILL_BATCH_SIZE;
        for (item_id, revision) in &candidates {
            if !is_text_free(&transaction, item_id)? {
                continue;
            }
            enqueue_for_new_item(&transaction, item_id, *revision, now)?;
            queued += 1;
        }
        transaction.commit()?;
        if !batch_was_full {
            return Ok(queued);
        }
    }
}

fn select_candidates(
    transaction: &Transaction<'_>,
    after_item_id: &str,
) -> anyhow::Result<Vec<(String, i32)>> {
    let mut statement = transaction.prepare(&format!(
        "SELECT i.item_id, i.revision
           FROM items i JOIN captures c ON c.capture_id = i.capture_id
          WHERE i.lifecycle_state != 'deleted'
            AND i.transcription_state = 'audio_pending'
            AND c.audio_reference IS NOT NULL
            AND i.item_id > ?
            AND NOT EXISTS (SELECT 1 FROM jobs j
                             WHERE j.item_id = i.item_id AND j.job_type = ?
                               AND j.status IN ({LIVE_OR_DONE_STATUSES}))
          ORDER BY i.item_id
          LIMIT ?"
    ))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                after_item_id,
                JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
                BACKFILL_BATCH_SIZE as i64
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Whether a transcript could still be attached: a corrected or typed text would make the job be
/// discarded, so queueing it again on every drain would be churn.
fn is_text_free(transaction: &Transaction<'_>, item_id: &str) -> anyhow::Result<bool> {
    let state = load_item_state(transaction, item_id)?
        .with_context(|| format!("item {item_id} disappeared during backfill"))?;
    Ok(matches!(
        validate_state_transition(&state, StateTransition::TranscriptAttached),
        TransitionValidity::Valid
    ))
}

/// Why a transcription retry was refused. Nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequeueRefusal {
    ItemDeleted,
    /// The audio was removed, so there is nothing to transcribe.
    AudioRemoved,
    /// The item already has its transcript, or is a text capture.
    NotRetryable(TranscriptionState),
    /// A transcription job for the item is already queued or running.
    JobInFlight,
}

#[derive(Debug)]
pub enum RequeueError {
    ItemNotFound,
    Refused(RequeueRefusal),
    Storage(anyhow::Error),
}

impl From<anyhow::Error> for RequeueError {
    fn from(error: anyhow::Error) -> Self {
        RequeueError::Storage(error)
    }
}

impl From<rusqlite::Error> for RequeueError {
    fn from(error: rusqlite::Error) -> Self {
        RequeueError::Storage(error.into())
    }
}

impl fmt::Display for RequeueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequeueError::ItemNotFound => write!(f, "item not found"),
            RequeueError::Refused(refusal) => write!(f, "transcription retry refused: {refusal:?}"),
            RequeueError::Storage(error) => write!(f, "storage failure: {error}"),
        }
    }
}

impl std::error::Error for RequeueError {}

/// Retry on-device transcription of one item. One transaction moves a `transcription_failed` or
/// `transcription_unsupported` item back to `audio_pending` and queues a new job at the item's
/// current revision. An item that is still `audio_pending` or `transcribing` with no job in
/// flight just gets the job. Returns the new job.
pub fn requeue_transcription(
    database: &mut Database,
    item_id: &str,
    now: DateTime<Utc>,
) -> Result<Job, RequeueError> {
    let transaction = database.immediate_transaction()?;
    let (revision, lifecycle_state, state, audio_reference): (i32, String, String, Option<String>) =
        transaction
            .query_row(
                "SELECT i.revision, i.lifecycle_state, i.transcription_state, c.audio_reference
                   FROM items i JOIN captures c ON c.capture_id = i.capture_id
                  WHERE i.item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(RequeueError::ItemNotFound)?;
    let state = state
        .parse::<TranscriptionState>()
        .map_err(|error| anyhow::anyhow!("stored transcription state is invalid: {}", error.0))?;

    let refuse = |refusal| Err(RequeueError::Refused(refusal));
    if lifecycle_state == "deleted" {
        return refuse(RequeueRefusal::ItemDeleted);
    }
    if matches!(
        state,
        TranscriptionState::Transcribed | TranscriptionState::NotApplicable
    ) {
        return refuse(RequeueRefusal::NotRetryable(state));
    }
    if audio_reference.is_none() {
        return refuse(RequeueRefusal::AudioRemoved);
    }
    let in_flight: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE item_id = ? AND job_type = ?
                         AND status IN ('queued', 'running'))",
        rusqlite::params![item_id, JOB_TYPE_TRANSCRIPTION_ATTACHMENT],
        |row| row.get(0),
    )?;
    if in_flight {
        return refuse(RequeueRefusal::JobInFlight);
    }

    transaction.execute(
        "UPDATE items SET transcription_state = ? WHERE item_id = ?",
        rusqlite::params![TranscriptionState::AudioPending.as_str(), item_id],
    )?;
    let job = enqueue_for_new_item(&transaction, item_id, revision, now)?;
    transaction.commit().map_err(anyhow::Error::from)?;
    Ok(job)
}
