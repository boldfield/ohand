//! Transactional foreground ingress import (C02a).
//!
//! One IMMEDIATE transaction stores the capture (reusing `store::captures`), creates the item
//! with its initial independent states, and writes the search-index row (`retrieval::index`).
//! The acknowledgment is returned only after that transaction has committed; every error
//! reports whether the commit happened, so the native side removes its staging record only on
//! `Ok` and keeps it (and retries with the same capture ID) otherwise.
//!
//! This is the Capture Ingestion Contract acknowledgment, not the pre-import capture write that
//! the `ohand_core_start_save_capture` export performs. Nothing in this module deletes a
//! capture, so a failed or interrupted import can never remove the sole copy of a source.

use crate::retrieval::index::sync_item_in_tx;
use crate::store::captures::{get_capture, save_capture_in_tx, Capture};
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use chrono::DateTime;
use chrono_tz::Tz;
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;

/// Largest absolute UTC offset (+/-18 hours) a time context may carry.
const MAX_UTC_OFFSET_MINUTES: i32 = 18 * 60;

/// Whether the import transaction is known to have committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitStatus {
    /// The transaction was rolled back or never started; nothing from this attempt persists.
    NotCommitted,
    /// The transaction committed; the item exists and a retry will return it.
    Committed,
    /// The commit call itself failed, so the outcome cannot be asserted. Retry with the same
    /// capture ID: the import is idempotent either way.
    Unknown,
}

/// Why an ingress record was refused before any write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngressRejection {
    MissingField(&'static str),
    MissingContent,
    UnsupportedScope(String),
    MalformedTimeContext(&'static str),
    MalformedAudioReference,
    MalformedSessionTopic,
    UnknownRoute(String),
    RouteScopeMismatch {
        route_id: String,
        route_scope: String,
        item_scope: String,
    },
}

#[derive(Debug)]
pub enum IngressErrorKind {
    /// The record is invalid; the error is specific and nothing was written.
    Validation(IngressRejection),
    /// The capture ID already holds different source content. The original is untouched.
    ConflictingReuse { capture_id: String },
    /// The capture was imported and later deleted; it is never recreated.
    ItemDeleted { item_id: String },
    /// A storage failure; the transaction is rolled back (or the commit outcome is unknown).
    Storage(anyhow::Error),
}

/// Import failure together with the commit status the caller must act on.
#[derive(Debug)]
pub struct IngressError {
    pub kind: IngressErrorKind,
    pub commit_status: CommitStatus,
}

impl IngressError {
    fn not_committed(kind: IngressErrorKind) -> Self {
        IngressError {
            kind,
            commit_status: CommitStatus::NotCommitted,
        }
    }

    fn storage(error: impl Into<anyhow::Error>, commit_status: CommitStatus) -> Self {
        IngressError {
            kind: IngressErrorKind::Storage(error.into()),
            commit_status,
        }
    }
}

impl fmt::Display for IngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            IngressErrorKind::Validation(rejection) => {
                write!(f, "ingress record rejected: {rejection:?}")?
            }
            IngressErrorKind::ConflictingReuse { capture_id } => write!(
                f,
                "capture ID {capture_id} already exists with different content"
            )?,
            IngressErrorKind::ItemDeleted { item_id } => {
                write!(f, "item {item_id} was deleted and is not re-imported")?
            }
            IngressErrorKind::Storage(error) => write!(f, "storage failure: {error}")?,
        }
        write!(f, " (commit status: {:?})", self.commit_status)
    }
}

impl std::error::Error for IngressError {}

/// Whether this call created the item or found the committed result of an earlier call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportDisposition {
    Imported,
    AlreadyImported,
}

/// Capture Ingestion Contract acknowledgment: returned only after the import committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngressImportAcknowledgment {
    pub capture_id: String,
    pub item_id: String,
    /// The capture's `created_at`, which is also the item's creation time, so every retry
    /// reports the same value.
    pub saved_at: String,
    pub disposition: ImportDisposition,
}

/// Points at which a test can fail the import to prove rollback and retry behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportStage {
    CaptureWritten,
    ItemWritten,
    IndexWritten,
    Committed,
}

/// Import a foreground ingress record in one durable, idempotent transaction.
///
/// * Identical retry (same ID and content): returns the existing item, creates nothing.
/// * Same ID, different content: `ConflictingReuse`; the stored capture is untouched.
/// * Invalid record or unknown route: a specific `Validation` error before any write.
/// * Any failure leaves no partial write; the error says whether the commit happened.
pub fn import_foreground_ingress(
    database: &mut Database,
    capture: &Capture,
) -> Result<IngressImportAcknowledgment, IngressError> {
    import_with_fault_points(database, capture, &mut |_| Ok(()))
}

pub(crate) fn import_with_fault_points(
    database: &mut Database,
    capture: &Capture,
    fault_point: &mut dyn FnMut(ImportStage) -> anyhow::Result<()>,
) -> Result<IngressImportAcknowledgment, IngressError> {
    validate_record(capture).map_err(|rejection| {
        IngressError::not_committed(IngressErrorKind::Validation(rejection))
    })?;

    let transaction = database
        .immediate_transaction()
        .map_err(|error| IngressError::storage(error, CommitStatus::NotCommitted))?;
    let before_commit =
        |error: anyhow::Error| IngressError::storage(error, CommitStatus::NotCommitted);

    let acknowledgment = match find_existing_item(&transaction, capture)? {
        Some(existing) => existing,
        None => {
            require_route(&transaction, capture)?;
            save_capture_in_tx(&transaction, capture).map_err(before_commit)?;
            fault_point(ImportStage::CaptureWritten).map_err(before_commit)?;

            let item_id = uuid::Uuid::new_v4().to_string();
            insert_item(&transaction, &item_id, capture)
                .map_err(|error| before_commit(error.into()))?;
            fault_point(ImportStage::ItemWritten).map_err(before_commit)?;

            sync_item_in_tx(&transaction, &item_id).map_err(before_commit)?;
            fault_point(ImportStage::IndexWritten).map_err(before_commit)?;

            IngressImportAcknowledgment {
                capture_id: capture.capture_id.clone(),
                item_id,
                saved_at: capture.created_at.clone(),
                disposition: ImportDisposition::Imported,
            }
        }
    };

    transaction
        .commit()
        .map_err(|error| IngressError::storage(error, CommitStatus::Unknown))?;
    fault_point(ImportStage::Committed)
        .map_err(|error| IngressError::storage(error, CommitStatus::Committed))?;
    Ok(acknowledgment)
}

/// Structural validation that needs no database. Whitespace-only text is accepted as given;
/// only absent or empty content is "null content".
fn validate_record(capture: &Capture) -> Result<(), IngressRejection> {
    let required_fields = [
        ("capture_id", &capture.capture_id),
        ("capture_instant", &capture.capture_instant),
        ("timezone_id", &capture.timezone_id),
        ("locale", &capture.locale),
        ("calendar", &capture.calendar),
        ("route_id", &capture.route_id),
        ("created_at", &capture.created_at),
    ];
    if let Some((name, _)) = required_fields.iter().find(|(_, value)| value.is_empty()) {
        return Err(IngressRejection::MissingField(name));
    }

    let non_empty = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.is_empty());
    if !non_empty(&capture.text) && !non_empty(&capture.audio_reference) {
        return Err(IngressRejection::MissingContent);
    }
    if let Some(reference) = &capture.audio_reference {
        let escapes_staging = reference.split(['/', '\\']).any(|segment| segment == "..");
        if reference.is_empty() || reference.contains('\0') || escapes_staging {
            return Err(IngressRejection::MalformedAudioReference);
        }
    }
    if capture.session_topic.as_deref().is_some_and(str::is_empty) {
        return Err(IngressRejection::MalformedSessionTopic);
    }

    capture
        .item_scope
        .parse::<ItemScope>()
        .map_err(|_| IngressRejection::UnsupportedScope(capture.item_scope.clone()))?;

    if DateTime::parse_from_rfc3339(&capture.capture_instant).is_err() {
        return Err(IngressRejection::MalformedTimeContext("capture_instant"));
    }
    if DateTime::parse_from_rfc3339(&capture.created_at).is_err() {
        return Err(IngressRejection::MalformedTimeContext("created_at"));
    }
    if capture.timezone_id.parse::<Tz>().is_err() {
        return Err(IngressRejection::MalformedTimeContext("timezone_id"));
    }
    if capture.utc_offset_minutes.abs() > MAX_UTC_OFFSET_MINUTES {
        return Err(IngressRejection::MalformedTimeContext("utc_offset_minutes"));
    }
    Ok(())
}

/// Resolves a previously committed import of this capture ID, rejecting changed content.
fn find_existing_item(
    transaction: &Transaction<'_>,
    capture: &Capture,
) -> Result<Option<IngressImportAcknowledgment>, IngressError> {
    let storage = |error: anyhow::Error| IngressError::storage(error, CommitStatus::NotCommitted);

    let existing_item: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT item_id, lifecycle_state, created_at FROM items WHERE capture_id = ?",
            [capture.capture_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| storage(error.into()))?;

    if let Some((item_id, lifecycle_state, _)) = &existing_item {
        if lifecycle_state == "deleted" {
            return Err(IngressError::not_committed(IngressErrorKind::ItemDeleted {
                item_id: item_id.clone(),
            }));
        }
    }

    let stored_capture = get_capture(transaction, &capture.capture_id).map_err(storage)?;
    if stored_capture
        .as_ref()
        .is_some_and(|stored| stored != capture)
    {
        return Err(conflicting_reuse(capture));
    }

    Ok(
        existing_item.map(|(item_id, _, saved_at)| IngressImportAcknowledgment {
            capture_id: capture.capture_id.clone(),
            item_id,
            saved_at,
            disposition: ImportDisposition::AlreadyImported,
        }),
    )
}

fn conflicting_reuse(capture: &Capture) -> IngressError {
    IngressError::not_committed(IngressErrorKind::ConflictingReuse {
        capture_id: capture.capture_id.clone(),
    })
}

fn require_route(transaction: &Transaction<'_>, capture: &Capture) -> Result<(), IngressError> {
    let route_scope: Option<String> = transaction
        .query_row(
            "SELECT scope FROM routes WHERE route_id = ?",
            [capture.route_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| IngressError::storage(error, CommitStatus::NotCommitted))?;

    let rejection = match route_scope {
        None => IngressRejection::UnknownRoute(capture.route_id.clone()),
        Some(scope) if scope != capture.item_scope => IngressRejection::RouteScopeMismatch {
            route_id: capture.route_id.clone(),
            route_scope: scope,
            item_scope: capture.item_scope.clone(),
        },
        Some(_) => return Ok(()),
    };
    Err(IngressError::not_committed(IngressErrorKind::Validation(
        rejection,
    )))
}

fn insert_item(
    transaction: &Transaction<'_>,
    item_id: &str,
    capture: &Capture,
) -> rusqlite::Result<usize> {
    let text_present = capture.text.as_deref().is_some_and(|text| !text.is_empty());
    let transcription_state = if text_present {
        "not_applicable"
    } else {
        "audio_pending"
    };
    transaction.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                            sync_state, processing_state, transcription_state,
                            created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', ?, ?, ?)",
        rusqlite::params![
            item_id,
            capture.capture_id,
            transcription_state,
            capture.created_at,
            capture.created_at,
        ],
    )
}

#[cfg(test)]
mod tests;
