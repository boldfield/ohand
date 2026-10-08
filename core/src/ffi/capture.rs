//! Durable capture save/read and item status read over the core handle (B01c).
//!
//! Each export queues one operation on the handle's worker thread and returns at once; the
//! outcome arrives through the registered event callback with the operation ID, as for
//! `ohand_core_start_store_check`. A request that is malformed is refused synchronously, before
//! anything is queued. Everything below the boundary is the existing store API: captures go
//! through `store::captures`, statuses through `domain::status::ItemStatus`, and failures are
//! normalized by `core_handle::failure`.
//!
//! * Save acknowledges only after the immediate transaction has committed. Any failure rolls the
//!   whole transaction back, so a failed save leaves neither a capture nor an item behind.
//! * Saving the identical capture again (same `created_at` too) succeeds with
//!   `already_saved: true`; the same ID with different content fails `capture_conflict` and
//!   the stored capture is untouched.
//! * Saving writes only the capture record. Item identity, the projection and the text index
//!   belong to the foreground import (C02a); this module neither chooses item IDs nor creates
//!   items. Statuses are read for items that the import has created.

use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::domain::status::ItemStatus;
use crate::store::captures::{get_capture, save_capture_in_tx, Capture};
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use anyhow::Error;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Longest request accepted by the capture exports, in bytes.
pub const OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES: usize = 1_048_576;

/// A capture as it crosses the boundary. `created_at` is the idempotency timestamp: a retry
/// of the same save must send the same value.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureRecord {
    capture_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    audio_reference: Option<String>,
    capture_instant: String,
    timezone_id: String,
    utc_offset_minutes: i32,
    locale: String,
    calendar: String,
    item_scope: String,
    route_id: String,
    entry_locked: bool,
    created_at: String,
    #[serde(default)]
    session_topic: Option<String>,
}

impl CaptureRecord {
    fn from_capture(capture: Capture) -> CaptureRecord {
        CaptureRecord {
            capture_id: capture.capture_id,
            text: capture.text,
            audio_reference: capture.audio_reference,
            capture_instant: capture.capture_instant,
            timezone_id: capture.timezone_id,
            utc_offset_minutes: capture.utc_offset_minutes,
            locale: capture.locale,
            calendar: capture.calendar,
            item_scope: capture.item_scope,
            route_id: capture.route_id,
            entry_locked: capture.entry_locked,
            created_at: capture.created_at,
            session_topic: capture.session_topic,
        }
    }

    /// Boundary validation only: required fields, content, and the closed scope vocabulary.
    /// No route store exists yet, so route existence and time-context semantics are enforced by
    /// the foreground import (C02a), which is the acknowledgment point for ingress.
    fn into_valid_capture(self) -> Result<Capture, AbiFailure> {
        let has_content = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
        let required_fields = [
            &self.capture_id,
            &self.capture_instant,
            &self.timezone_id,
            &self.locale,
            &self.calendar,
            &self.route_id,
            &self.created_at,
        ];
        if required_fields.iter().any(|field| field.is_empty())
            || !(has_content(&self.text) || has_content(&self.audio_reference))
            || self.item_scope.parse::<ItemScope>().is_err()
        {
            return Err(AbiFailure::INVALID_CAPTURE);
        }
        Capture::new(
            self.capture_id,
            self.text,
            self.audio_reference,
            self.capture_instant,
            self.timezone_id,
            self.utc_offset_minutes,
            self.locale,
            self.calendar,
            self.item_scope,
            self.route_id,
            self.entry_locked,
            self.created_at,
            self.session_topic,
        )
        .map_err(|_| AbiFailure::INVALID_CAPTURE)
    }
}

#[derive(Serialize)]
struct SaveAcknowledgment {
    operation_id: u64,
    already_saved: bool,
    capture: CaptureRecord,
}

#[derive(Serialize)]
struct CaptureReadout {
    operation_id: u64,
    capture: CaptureRecord,
}

/// Independent facts about one item; absent reminder facts are `null`, not defaults.
#[derive(Serialize)]
struct StatusReadout {
    operation_id: u64,
    item_id: String,
    save_state: &'static str,
    sync_state: &'static str,
    processing_state: &'static str,
    transcription_state: &'static str,
    processing_job_status: Option<&'static str>,
    reminder_request_state: Option<&'static str>,
    reminder_schedule_state: Option<&'static str>,
    reminder_delivery_state: Option<&'static str>,
    reminder_acknowledgment_state: Option<&'static str>,
    unschedulable_reason: Option<&'static str>,
}

fn storage_failure(error: impl Into<Error>) -> AbiFailure {
    AbiFailure::from_store(&error.into(), AbiFailure::STORAGE_ERROR)
}

fn to_json(value: &impl Serialize) -> Result<Vec<u8>, AbiFailure> {
    serde_json::to_vec(value).map_err(|_| AbiFailure::INTERNAL)
}

/// Copies the caller's bytes. The length is checked before the pointer is read.
///
/// # Safety
/// `bytes` must point at `len` readable bytes (it may be null only when `len` is 0).
unsafe fn copy_request(bytes: *const u8, len: usize) -> Result<Vec<u8>, AbiFailure> {
    if len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if bytes.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    Ok(std::slice::from_raw_parts(bytes, len).to_vec())
}

unsafe fn copy_identifier(bytes: *const u8, len: usize) -> Result<String, AbiFailure> {
    String::from_utf8(copy_request(bytes, len)?).map_err(|_| AbiFailure::INVALID_UTF8)
}

fn save_capture_job(capture: Capture, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let tx = database.immediate_transaction().map_err(storage_failure)?;
        let existing = get_capture(&tx, &capture.capture_id).map_err(storage_failure)?;
        let already_saved = match &existing {
            Some(stored) if *stored == capture => true,
            Some(_) => return Err(AbiFailure::CAPTURE_CONFLICT),
            None => false,
        };
        let saved = save_capture_in_tx(&tx, &capture).map_err(storage_failure)?;
        tx.commit().map_err(storage_failure)?;
        to_json(&SaveAcknowledgment {
            operation_id,
            already_saved,
            capture: CaptureRecord::from_capture(saved),
        })
    })
}

fn get_capture_job(capture_id: String, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let tx = database.transaction().map_err(storage_failure)?;
        let capture = get_capture(&tx, &capture_id)
            .map_err(storage_failure)?
            .ok_or(AbiFailure::NOT_FOUND)?;
        to_json(&CaptureReadout {
            operation_id,
            capture: CaptureRecord::from_capture(capture),
        })
    })
}

fn item_status_job(item_id: String, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let tx = database.transaction().map_err(storage_failure)?;
        let status = ItemStatus::load(&tx, &item_id)
            .map_err(storage_failure)?
            .ok_or(AbiFailure::NOT_FOUND)?;
        to_json(&StatusReadout {
            operation_id,
            item_id: status.item_id,
            save_state: status.save_state.as_str(),
            sync_state: status.sync_state.as_str(),
            processing_state: status.processing_state.as_str(),
            transcription_state: status.transcription_state.as_str(),
            processing_job_status: status.processing_job_status.map(|value| value.as_str()),
            reminder_request_state: status.reminder_request_state.map(|value| value.as_str()),
            reminder_schedule_state: status.reminder_schedule_state.map(|value| value.as_str()),
            reminder_delivery_state: status.reminder_delivery_state.map(|value| value.as_str()),
            reminder_acknowledgment_state: status
                .reminder_acknowledgment_state
                .map(|value| value.as_str()),
            unschedulable_reason: status.unschedulable_reason.map(|value| value.as_str()),
        })
    })
}

/// Queues a durable save of the capture described by the `request_len` JSON bytes at
/// `request` (fields of `CaptureRecord`; unknown fields are rejected). The outcome event for
/// `operation_id` is success only after the save committed, with JSON
/// `{"operation_id","already_saved","capture"}`, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_save_capture(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = copy_request(request, request_len)?;
        let record: CaptureRecord =
            serde_json::from_slice(&bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let capture = record.into_valid_capture()?;
        core.submit(operation_id, save_capture_job(capture, operation_id))
    })
}

/// Queues a read of the stored capture whose ID is the `capture_id_len` UTF-8 bytes at
/// `capture_id`. The outcome event holds `{"operation_id","capture"}`, or `not_found`.
///
/// # Safety
/// `capture_id` must point at `capture_id_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_get_capture(
    handle: OhandCoreHandle,
    operation_id: u64,
    capture_id: *const u8,
    capture_id_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let capture_id = copy_identifier(capture_id, capture_id_len)?;
        core.submit(operation_id, get_capture_job(capture_id, operation_id))
    })
}

/// Queues a read of the independent save, sync, processing, transcription and reminder
/// states of the item whose ID is the `item_id_len` UTF-8 bytes at `item_id`. The outcome
/// event holds the `StatusReadout` JSON, or `not_found`.
///
/// # Safety
/// `item_id` must point at `item_id_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_item_status(
    handle: OhandCoreHandle,
    operation_id: u64,
    item_id: *const u8,
    item_id_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let item_id = copy_identifier(item_id, item_id_len)?;
        core.submit(operation_id, item_status_job(item_id, operation_id))
    })
}

#[cfg(test)]
mod tests;
