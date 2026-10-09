//! Durable reads for the dispatcher: the text a job interprets and the profile it is pinned to.

use super::RetireReason;
use crate::jobs::queue::Job;
use crate::providers::contracts::{ProviderProfile, TextBasis, PROFILE_SCHEMA_VERSION};
use crate::time::TimeContext;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

/// Everything a request needs from the item and its capture.
pub(super) struct InterpretationSource {
    pub item_id: String,
    pub capture_id: String,
    pub text: String,
    pub text_basis: TextBasis,
    pub time_context: TimeContext,
}

pub(super) enum SourceState {
    Ready(InterpretationSource),
    /// The job can never produce a usable result.
    Retired(RetireReason),
    /// The item exists but has no text yet (audio awaiting transcription).
    NoText,
}

type CaptureRow = (
    String,
    i32,
    String,
    Option<String>,
    String,
    String,
    i32,
    String,
    String,
);

/// Read the item's current effective text (the latest text correction, else the capture text)
/// together with the capture's time context.
pub(super) fn load_source(conn: &Connection, job: &Job) -> Result<SourceState> {
    let row: Option<CaptureRow> = conn
        .query_row(
            "SELECT i.capture_id, i.revision, i.lifecycle_state, c.text, c.capture_instant,
                    c.timezone_id, c.utc_offset_minutes, c.locale, c.calendar
               FROM items i JOIN captures c ON c.capture_id = i.capture_id
              WHERE i.item_id = ?",
            [&job.item_id],
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
        .context("reading item and capture")?;
    let Some((
        capture_id,
        revision,
        lifecycle_state,
        capture_text,
        capture_instant,
        timezone,
        offset_minutes,
        locale,
        calendar,
    )) = row
    else {
        return Ok(SourceState::Retired(RetireReason::ItemMissing));
    };
    if lifecycle_state != "active" {
        return Ok(SourceState::Retired(RetireReason::ItemNotActive));
    }
    if revision != job.source_revision {
        return Ok(SourceState::Retired(RetireReason::SourceRevised));
    }

    let latest_correction: Option<(String, String)> = conn
        .query_row(
            "SELECT correction_id, new_value FROM corrections
              WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
            [&job.item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("reading text correction")?;
    let item_revision = u64::try_from(revision).unwrap_or_default();
    let (text, text_basis) = match (latest_correction, capture_text) {
        (Some((correction_id, corrected_text)), _) => (
            corrected_text,
            TextBasis::Correction {
                correction_record_id: correction_id
                    .strip_suffix("-correction")
                    .unwrap_or(&correction_id)
                    .to_string(),
                item_revision,
            },
        ),
        (None, Some(original_text)) => (original_text, TextBasis::Original { item_revision }),
        (None, None) => return Ok(SourceState::NoText),
    };

    let reference_time = DateTime::parse_from_rfc3339(&capture_instant)
        .with_context(|| format!("malformed capture_instant {capture_instant:?}"))?
        .with_timezone(&Utc);
    Ok(SourceState::Ready(InterpretationSource {
        item_id: job.item_id.clone(),
        capture_id,
        text,
        text_basis,
        time_context: TimeContext {
            timezone,
            locale,
            reference_time,
            utc_offset_at_capture: offset_minutes.saturating_mul(60),
            calendar,
        },
    }))
}

type ProfileRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    String,
    String,
    String,
    String,
);

/// Rebuild the validated profile a job is pinned to from its stored row. A missing row, malformed
/// stored JSON or any contract violation yields `None`: the dispatcher then treats the profile as
/// unusable instead of guessing at its contents.
pub(super) fn load_profile(
    conn: &Connection,
    profile_version: &str,
) -> Result<Option<ProviderProfile>> {
    let row: Option<ProfileRow> = conn
        .query_row(
            "SELECT profile_id, provider_type, endpoint, model, credential_ref, timeout_seconds,
                    retry_policy, authorized_destinations, capabilities, created_at
               FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
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
                    row.get(9)?,
                ))
            },
        )
        .optional()
        .context("reading pinned profile")?;
    let Some((
        profile_id,
        provider_type,
        endpoint,
        model,
        credential_ref,
        timeout_seconds,
        retry_policy,
        authorized_destinations,
        capabilities,
        created_at,
    )) = row
    else {
        return Ok(None);
    };
    let parsed_json = |text: &str| serde_json::from_str::<Value>(text).ok();
    let (Some(retry_policy), Some(authorized_destinations), Some(capabilities)) = (
        parsed_json(&retry_policy),
        parsed_json(&authorized_destinations),
        parsed_json(&capabilities),
    ) else {
        return Ok(None);
    };
    let record = json!({
        "schema_version": PROFILE_SCHEMA_VERSION,
        "profile_version": profile_version,
        "profile_id": profile_id,
        "protocol": provider_type,
        "endpoint": endpoint,
        "model": model,
        "credential_ref": credential_ref,
        "timeout_seconds": timeout_seconds,
        "retry_policy": retry_policy,
        "authorized_destinations": authorized_destinations,
        "capabilities": capabilities,
        "created_at": created_at,
    });
    Ok(serde_json::from_value::<ProviderProfile>(record).ok())
}
