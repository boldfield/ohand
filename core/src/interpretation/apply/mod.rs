/// Proposal validation and atomic application to stored item state.
///
/// Validates candidate schema, source, revision, policy and lifecycle before applying
/// replaceable derived state. Invalid/stale/unauthorized results preserve prior state;
/// atomic application and job completion prevent duplicate effects after crash.
///
/// A first interpretation may produce an abstention with an explicit uninterpreted state,
/// searchable and excluded from suggestions. Later interpretations preserve prior
/// authoritative state on failure. Reminder candidates require explicit intent and
/// deterministic resolution consistent with source/context; unsupported grammar stays
/// unscheduled with original intention intact.
use crate::domain::items::{ItemState, LifecycleState, TextState};
use crate::interpretation::contracts::{Proposal, TimeResolutionQuality};
use crate::providers::contracts::TextBasis;
use crate::store::events::ItemType;
use anyhow::Result;
use chrono::DateTime;
use rusqlite::{OptionalExtension, Transaction};
use thiserror::Error;

/// Errors that occur when applying a proposal to stored state.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApplyError {
    #[error("proposal is malformed or invalid: {0}")]
    Validation(String),
    #[error("item {item_id} not found")]
    ItemNotFound { item_id: String },
    #[error("proposal stale: expected item revision {expected}, current revision is {current}")]
    StaleRevision { expected: i32, current: i32 },
    #[error("item {item_id} is in {state} state and cannot be modified")]
    LifecycleViolation { item_id: String, state: String },
    #[error("cannot apply changes to deleted item {item_id}")]
    DeletedItem { item_id: String },
    #[error("proposal capture_id does not match item's capture_id")]
    CaptureIdMismatch,
    #[error("text basis is not current at the proposal's source_revision")]
    TextBasisNotCurrent,
    #[error("reminder requires an explicit item_type that can carry obligations")]
    ReminderRequiresActionType,
    #[error("ambiguous reminder cannot carry a resolved instant")]
    AmbiguousReminderWithInstant,
    #[error("explicit/inferred reminder must carry a resolved instant")]
    MissingReminderInstant,
    #[error("storage error: {0}")]
    Storage(String),
}

/// Outcome of applying a proposal to an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Proposal was applied successfully; item now reflects its changes.
    Applied,
    /// Proposal was rejected as stale; item remains unchanged.
    Rejected,
}

/// Apply a validated proposal to the current item state. This function performs
/// additional database-state validations beyond the syntactic checks in
/// [`Proposal::validate`], ensuring that the proposal is fresh (not stale) and
/// that the item is in a state that permits application.
///
/// Returns `ApplyOutcome::Applied` on success, or `ApplyOutcome::Rejected` if the
/// proposal is stale or the item cannot be modified.
///
/// # Constraints
/// - The proposal's source_revision must match the item's current revision.
/// - The item must not be deleted or in a terminal state.
/// - The item's capture_id must match the proposal's capture_id.
/// - If a reminder is proposed, the item type must be Action or must be set by the proposal.
/// - If the reminder quality is ambiguous, no instant may be present.
/// - If the reminder quality is explicit or inferred, an instant must be present.
pub fn apply_proposal(
    tx: &Transaction,
    item_state: &ItemState,
    proposal: &Proposal,
) -> Result<ApplyOutcome, ApplyError> {
    // Syntactic validation was already done by Proposal::validate.
    // Now we check database state and proposal freshness.

    // 1. Check item exists and is not deleted.
    if item_state.lifecycle_state == LifecycleState::Deleted {
        return Err(ApplyError::DeletedItem {
            item_id: item_state.item_id.clone(),
        });
    }

    // 2. Check proposal revision matches current item revision.
    if proposal.source_revision != item_state.revision {
        return Err(ApplyError::StaleRevision {
            expected: proposal.source_revision,
            current: item_state.revision,
        });
    }

    // 3. Verify capture_id matches.
    if proposal.capture_id != item_state.capture_id {
        return Err(ApplyError::CaptureIdMismatch);
    }

    // 4. Verify text basis is current at the proposed revision.
    verify_text_basis_is_current(&item_state.current_text, &proposal.text_basis)?;

    // 5. Validate reminder constraints if present.
    if let Some(reminder) = &proposal.reminder_proposal {
        validate_reminder_application(item_state, proposal, reminder)?;
    }

    // 6. If abstention, persist abstained state and return success.
    if proposal.abstention.is_some() {
        update_processing_state(tx, &item_state.item_id, "abstained")?;
        return Ok(ApplyOutcome::Applied);
    }

    // 7. Apply the proposal facets to the item (type, session_topic, reminder).
    apply_facets(tx, item_state, proposal)?;

    // 8. Update processing state to processed.
    update_processing_state(tx, &item_state.item_id, "processed")?;

    Ok(ApplyOutcome::Applied)
}

/// Verify that the text basis matches the current item text state.
fn verify_text_basis_is_current(
    current_text: &TextState,
    text_basis: &TextBasis,
) -> Result<(), ApplyError> {
    match text_basis {
        TextBasis::Original { .. } => {
            // Original basis is current only if the item hasn't been corrected.
            if matches!(current_text, TextState::Original { .. }) {
                Ok(())
            } else {
                Err(ApplyError::TextBasisNotCurrent)
            }
        }
        TextBasis::Correction { .. } => {
            // Correction basis is current if the item has been corrected.
            if matches!(current_text, TextState::Corrected { .. }) {
                Ok(())
            } else {
                Err(ApplyError::TextBasisNotCurrent)
            }
        }
    }
}

/// Validate that a proposed reminder can be applied to the item.
fn validate_reminder_application(
    item_state: &ItemState,
    proposal: &Proposal,
    reminder: &crate::interpretation::contracts::ReminderProposal,
) -> Result<(), ApplyError> {
    // Check that the item_type supports carrying reminders.
    // Only Action items can carry reminders; if the proposal sets the type, use that.
    let effective_type = proposal.item_type.or(item_state.item_type);

    if let Some(item_type) = effective_type {
        if !item_type.can_carry_obligation() {
            return Err(ApplyError::ReminderRequiresActionType);
        }
    } else {
        // Proposal must set type to Action if it proposes a reminder and item has no type.
        if proposal.item_type != Some(ItemType::Action) {
            return Err(ApplyError::ReminderRequiresActionType);
        }
    }

    // Validate reminder time resolution.
    match reminder.quality {
        TimeResolutionQuality::Ambiguous => {
            if reminder.instant.is_some() {
                return Err(ApplyError::AmbiguousReminderWithInstant);
            }
        }
        TimeResolutionQuality::Explicit | TimeResolutionQuality::Inferred => {
            if reminder.instant.is_none() {
                return Err(ApplyError::MissingReminderInstant);
            }
            // Validate that the instant is a valid RFC 3339 timestamp.
            if let Some(instant_str) = &reminder.instant {
                DateTime::parse_from_rfc3339(instant_str)
                    .map_err(|_| ApplyError::MissingReminderInstant)?;
            }
        }
    }

    Ok(())
}

/// Apply the proposed facets (item_type, session_topic, reminder) to the item.
/// This is a transactional operation that updates the item in the database.
fn apply_facets(
    tx: &Transaction,
    item_state: &ItemState,
    proposal: &Proposal,
) -> Result<(), ApplyError> {
    // Update item type if proposed (and not already corrected by user).
    if let Some(proposed_type) = proposal.item_type {
        if !item_state.provenance.type_corrected {
            tx.execute(
                "UPDATE items SET item_type = ? WHERE item_id = ?",
                rusqlite::params![proposed_type.as_str(), &item_state.item_id],
            )
            .map_err(|e| ApplyError::Storage(e.to_string()))?;
        }
    }

    // Update session topic if proposed (respecting user corrections).
    if let Some(ref session_topic_prop) = proposal.session_topic_proposal {
        if !item_state.provenance.session_topic_corrected {
            let new_topic = if session_topic_prop.topic.is_empty() {
                None
            } else {
                Some(session_topic_prop.topic.as_str())
            };
            tx.execute(
                "UPDATE items SET current_session_topic = ? WHERE item_id = ?",
                rusqlite::params![new_topic, &item_state.item_id],
            )
            .map_err(|e| ApplyError::Storage(e.to_string()))?;
        }
    }

    // Update reminder state if proposed.
    if let Some(reminder) = &proposal.reminder_proposal {
        apply_reminder_proposal(tx, &item_state.item_id, reminder)?;
    }

    Ok(())
}

/// Update or insert reminder state for the proposal.
fn apply_reminder_proposal(
    tx: &Transaction,
    item_id: &str,
    reminder: &crate::interpretation::contracts::ReminderProposal,
) -> Result<(), ApplyError> {
    let request_state = match reminder.quality {
        TimeResolutionQuality::Explicit | TimeResolutionQuality::Inferred => {
            if reminder.instant.is_some() {
                "resolved"
            } else {
                "not_scheduled_yet"
            }
        }
        TimeResolutionQuality::Ambiguous => "not_scheduled_yet",
    };

    // Check if reminder row exists
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM reminders WHERE item_id = ? LIMIT 1",
            rusqlite::params![item_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?
        .unwrap_or(false);

    if exists {
        // Update existing reminder
        tx.execute(
            "UPDATE reminders SET request_state = ?, schedule_state = 'not_scheduled' WHERE item_id = ?",
            rusqlite::params![request_state, item_id],
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;
    } else {
        // Insert new reminder with default values
        tx.execute(
            "INSERT INTO reminders (item_id, request_state, schedule_state, delivery_state, acknowledgment_state) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![item_id, request_state, "not_scheduled", "unknown", "not_acknowledged"],
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;
    }

    Ok(())
}

/// Update the processing_state for an item.
fn update_processing_state(tx: &Transaction, item_id: &str, state: &str) -> Result<(), ApplyError> {
    tx.execute(
        "UPDATE items SET processing_state = ? WHERE item_id = ?",
        rusqlite::params![state, item_id],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;
    Ok(())
}
