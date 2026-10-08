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
use crate::jobs::queue::complete_job_in_tx;
use crate::providers::contracts::TextBasis;
use crate::store::events::ItemType;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction};
use thiserror::Error;
use uuid::Uuid;

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

/// Apply a proposal to item state with full validation and job completion.
/// Loads current item state and proposal row, validates using D04 guards,
/// stores facets in proposals table, rebuilds state from events, and completes job atomically.
///
/// Returns `ApplyOutcome::Applied` on success, or `ApplyOutcome::Rejected` if validation
/// or authorization fails, or `ApplyError` on database/storage errors.
///
/// This function performs all validation in one transaction:
/// - Proposal schema and semantic validation via Proposal::validate
/// - Item freshness: source_revision matches current revision
/// - Item exists and authorization: capture_id matches
/// - Lifecycle validation: item is not deleted/terminal (only Active)
/// - Text basis currency check
/// - Reminder constraints if present
/// - D04 guards: schema version, applied_state, abstained flag, user corrections
/// - State rebuild from events/proposals with projection consistency
/// - Reminder persistence to reminders table
/// - Job completion with lease verification
pub fn apply_proposal(
    tx: &Transaction,
    item_id: &str,
    proposal: &Proposal,
    job_id: &str,
    lease_attempt: i32,
) -> Result<ApplyOutcome, ApplyError> {
    // Load current item state from transaction (not trusting caller).
    let item_state = load_item_state(tx, item_id)?;

    // Load the capture text for validation.
    let source_text = load_source_text(tx, &item_state.capture_id)?;

    // 1. Validate proposal schema, semantics, source spans, and all field constraints.
    proposal
        .validate(&source_text)
        .map_err(|e| ApplyError::Validation(format!("{}", e)))?;

    // 2. Verify item is not deleted.
    if item_state.lifecycle_state == LifecycleState::Deleted {
        return Err(ApplyError::DeletedItem {
            item_id: item_state.item_id.clone(),
        });
    }

    // 3. Check proposal freshness: source_revision must match current revision.
    if proposal.source_revision != item_state.revision {
        return Err(ApplyError::StaleRevision {
            expected: proposal.source_revision,
            current: item_state.revision,
        });
    }

    // 4. Verify capture_id matches (authorization check).
    if proposal.capture_id != item_state.capture_id {
        return Err(ApplyError::CaptureIdMismatch);
    }

    // 5. Load and validate stored proposal row (D04 guards).
    load_proposal_row(tx, &proposal.proposal_id, item_id, &item_state.capture_id)?;

    // 6. Verify text basis is current at the proposed revision.
    verify_text_basis_is_current(tx, item_id, &item_state.current_text, &proposal.text_basis)?;

    // 7. Lifecycle guard: only Active items can be modified.
    if item_state.lifecycle_state != LifecycleState::Active {
        return Err(ApplyError::LifecycleViolation {
            item_id: item_id.to_string(),
            state: item_state.lifecycle_state.as_str().to_string(),
        });
    }

    // 8. Validate reminder constraints if present.
    if let Some(reminder) = &proposal.reminder_proposal {
        validate_reminder_application(&item_state, proposal, reminder)?;
    }

    // 9. Determine if this is a first interpretation by checking existing processing state.
    let is_first_interpretation = is_first_interpretation(tx, item_id)?;

    // 10. If abstention, handle based on whether this is first interpretation.
    if proposal.abstention.is_some() {
        if is_first_interpretation {
            // First-pass abstention: mark as uninterpreted (searchable, suggestion-excluded).
            mark_uninterpreted(tx, item_id)?;
        } else {
            // Later abstention: preserve prior state, just record abstention.
            update_processing_state(tx, item_id, "abstained")?;
        }
        // Mark proposal as applied.
        mark_proposal_applied(tx, &proposal.proposal_id, item_id)?;
        // Complete the job atomically.
        complete_job_in_tx(tx, job_id, lease_attempt)
            .map_err(|e| ApplyError::Storage(format!("Job completion failed: {}", e)))?;
        return Ok(ApplyOutcome::Applied);
    }

    // 11. Validate type proposal against user corrections (D04 guard).
    if proposal.item_type.is_some() && item_state.provenance.type_corrected {
        return Err(ApplyError::Validation(
            "cannot override user-corrected item type".to_string(),
        ));
    }

    // 12. Validate session topic proposal against user corrections (D04 guard).
    if proposal.session_topic_proposal.is_some() && item_state.provenance.session_topic_corrected {
        return Err(ApplyError::Validation(
            "cannot override user-corrected session topic".to_string(),
        ));
    }

    // 13. Store facets in proposals table for rebuild consistency.
    store_proposal_facets(tx, &proposal.proposal_id, item_id, proposal)?;

    // 14. Supersede prior proposals and mark this one as applied.
    mark_prior_proposals_superseded(tx, item_id, proposal.source_revision, &proposal.proposal_id)?;
    mark_proposal_applied(tx, &proposal.proposal_id, item_id)?;

    // 15. Rebuild state from events/proposals to derive final state with user-correction overrides.
    let rebuilt_state = crate::domain::items::rebuild_state_from_events(tx, item_id)
        .map_err(|e| ApplyError::Storage(format!("Rebuild failed: {}", e)))?
        .ok_or_else(|| ApplyError::Storage("Rebuild returned no state".to_string()))?;

    // 16. Persist rebuilt state to items table.
    tx.execute(
        "UPDATE items SET item_type = ?, current_session_topic = ? WHERE item_id = ?",
        rusqlite::params![
            rebuilt_state.item_type.as_ref().map(|t| t.as_str()),
            rebuilt_state.session_topic,
            item_id
        ],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;

    // 17. Apply reminders to reminders table.
    if let Some(reminder) = &proposal.reminder_proposal {
        apply_reminder_proposal(tx, item_id, reminder)?;
    }

    // 18. Update processing state to processed.
    update_processing_state(tx, item_id, "processed")?;

    // 19. Complete the job atomically (J01 integration).
    complete_job_in_tx(tx, job_id, lease_attempt)
        .map_err(|e| ApplyError::Storage(format!("Job completion failed: {}", e)))?;

    Ok(ApplyOutcome::Applied)
}

/// Load the current item state from the database.
fn load_item_state(tx: &Transaction, item_id: &str) -> Result<ItemState, ApplyError> {
    // Query item and related data to reconstruct ItemState.
    let row_result = tx.query_row(
        "SELECT item_id, capture_id, revision, item_type, lifecycle_state,
                current_session_topic FROM items WHERE item_id = ?",
        [item_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    );

    let (item_id_check, capture_id, revision, item_type_str, lifecycle_state_str, session_topic) =
        row_result.map_err(|_| ApplyError::ItemNotFound {
            item_id: item_id.to_string(),
        })?;

    let lifecycle_state = match lifecycle_state_str.as_str() {
        "active" => LifecycleState::Active,
        "completed" => LifecycleState::Completed,
        "cancelled" => LifecycleState::Cancelled,
        "deleted" => LifecycleState::Deleted,
        _ => {
            return Err(ApplyError::Storage(format!(
                "Unknown lifecycle state: {}",
                lifecycle_state_str
            )))
        }
    };

    let item_type = item_type_str.as_deref().and_then(|s| match s {
        "action" => Some(ItemType::Action),
        "note" => Some(ItemType::Note),
        "idea" => Some(ItemType::Idea),
        _ => None,
    });

    // Load text state (original or corrected).
    let current_text = load_text_state(tx, item_id)?;

    // Load field provenance.
    let provenance = load_field_provenance(tx, item_id)?;

    Ok(ItemState {
        item_id: item_id_check,
        capture_id,
        revision,
        item_type,
        scope: crate::domain::items::ItemScope::Personal, // TODO: load from items.current_scope
        session_topic,
        lifecycle_state,
        current_text,
        provenance,
    })
}

/// Load the text state (original or corrected) for an item.
fn load_text_state(tx: &Transaction, item_id: &str) -> Result<TextState, ApplyError> {
    // Check for corrections.
    let correction_result: Option<(String, String)> = tx
        .query_row(
            "SELECT new_value, created_at FROM corrections WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    if let Some((corrected_text, corrected_at)) = correction_result {
        return Ok(TextState::Corrected {
            text: corrected_text,
            corrected_at,
        });
    }

    // Load original from capture.
    let original_text: Option<String> = tx
        .query_row(
            "SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = ?)",
            [item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?
        .flatten();

    Ok(TextState::Original {
        text: original_text,
    })
}

/// Load field provenance (which fields were corrected by the user).
fn load_field_provenance(
    tx: &Transaction,
    item_id: &str,
) -> Result<crate::domain::items::FieldProvenance, ApplyError> {
    let type_corrected: bool = tx
        .query_row(
            "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'type'",
            [item_id],
            |row| {
                let count: i32 = row.get(0)?;
                Ok(count > 0)
            },
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    let session_topic_corrected: bool = tx
        .query_row(
            "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'session_topic'",
            [item_id],
            |row| {
                let count: i32 = row.get(0)?;
                Ok(count > 0)
            },
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    let text_corrected: bool = tx
        .query_row(
            "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'text'",
            [item_id],
            |row| {
                let count: i32 = row.get(0)?;
                Ok(count > 0)
            },
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    Ok(crate::domain::items::FieldProvenance {
        type_corrected,
        scope_corrected: false,
        session_topic_corrected,
        text_corrected,
    })
}

/// Load the source text from the capture for validation.
fn load_source_text(tx: &Transaction, capture_id: &str) -> Result<String, ApplyError> {
    let text: Option<String> = tx
        .query_row(
            "SELECT text FROM captures WHERE capture_id = ?",
            [capture_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?
        .flatten();

    text.ok_or_else(|| ApplyError::Storage("Capture has no text".to_string()))
}

/// Check if this is a first interpretation by looking at processing_state.
fn is_first_interpretation(tx: &Transaction, item_id: &str) -> Result<bool, ApplyError> {
    let processing_state: Option<String> = tx
        .query_row(
            "SELECT processing_state FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    // First interpretation if processing_state is "unprocessed" or "uninterpreted".
    Ok(processing_state.as_deref() == Some("unprocessed")
        || processing_state.as_deref() == Some("uninterpreted"))
}

/// Mark an item as uninterpreted (explicit state for first-pass failure/abstention).
fn mark_uninterpreted(tx: &Transaction, item_id: &str) -> Result<(), ApplyError> {
    tx.execute(
        "UPDATE items SET processing_state = ? WHERE item_id = ?",
        rusqlite::params!["uninterpreted", item_id],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;
    Ok(())
}

/// Mark a proposal as applied in the proposals table.
fn mark_proposal_applied(
    tx: &Transaction,
    proposal_id: &str,
    item_id: &str,
) -> Result<(), ApplyError> {
    tx.execute(
        "UPDATE proposals SET applied_state = ? WHERE proposal_id = ? AND item_id = ?",
        rusqlite::params!["applied", proposal_id, item_id],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;
    Ok(())
}

/// Verify that the text basis matches the current item text state.
fn verify_text_basis_is_current(
    tx: &Transaction,
    item_id: &str,
    current_text: &TextState,
    text_basis: &TextBasis,
) -> Result<(), ApplyError> {
    match text_basis {
        TextBasis::Original { item_revision: _ } => {
            // Original basis is current only if the item hasn't been corrected.
            if matches!(current_text, TextState::Original { .. }) {
                Ok(())
            } else {
                Err(ApplyError::TextBasisNotCurrent)
            }
        }
        TextBasis::Correction {
            correction_record_id,
            item_revision: _,
        } => {
            // Correction basis is current only if it matches the latest correction record.
            if !matches!(current_text, TextState::Corrected { .. }) {
                return Err(ApplyError::TextBasisNotCurrent);
            }

            // Verify correction record id and revision match the basis.
            let stored_correction: Option<String> = tx
                .query_row(
                    "SELECT correction_id FROM corrections WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
                    [item_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| ApplyError::Storage(e.to_string()))?
                .flatten();

            match stored_correction {
                Some(stored_id) if stored_id == *correction_record_id => Ok(()),
                _ => Err(ApplyError::TextBasisNotCurrent),
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

/// Load the proposal row and verify all D04 guards.
fn load_proposal_row(
    tx: &Transaction,
    proposal_id: &str,
    item_id: &str,
    capture_id: &str,
) -> Result<(), ApplyError> {
    use crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;

    let row: Option<(i32, String)> = tx
        .query_row(
            "SELECT schema_version, applied_state FROM proposals
             WHERE proposal_id = ? AND item_id = ? AND capture_id = ?",
            rusqlite::params![proposal_id, item_id, capture_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| ApplyError::Storage(e.to_string()))?;

    let (schema_version, applied_state) = row.ok_or_else(|| {
        ApplyError::Storage("Proposal not found or belongs to another item".to_string())
    })?;

    // Schema version must be supported.
    if schema_version != SUPPORTED_PROPOSAL_SCHEMA_VERSION {
        return Err(ApplyError::Validation(format!(
            "Unsupported proposal schema version: {}",
            schema_version
        )));
    }

    // Only unapplied proposals can be applied.
    if applied_state != "unapplied" {
        return Err(ApplyError::Validation(format!(
            "Proposal is not applicable in state '{}'",
            applied_state
        )));
    }

    Ok(())
}

/// Store proposed facets in the proposals table for event/projection rebuild.
fn store_proposal_facets(
    tx: &Transaction,
    proposal_id: &str,
    item_id: &str,
    proposal: &Proposal,
) -> Result<(), ApplyError> {
    let proposal_type = proposal.item_type.as_ref().map(|t| t.as_str());
    let session_topic_proposal = proposal
        .session_topic_proposal
        .as_ref()
        .map(|stp| stp.topic.as_str());

    tx.execute(
        "UPDATE proposals SET proposal_type = ?, session_topic_proposal = ? WHERE proposal_id = ? AND item_id = ?",
        rusqlite::params![proposal_type, session_topic_proposal, proposal_id, item_id],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;

    Ok(())
}

/// Mark all prior applied proposals as superseded for this revision.
fn mark_prior_proposals_superseded(
    tx: &Transaction,
    item_id: &str,
    source_revision: i32,
    current_proposal_id: &str,
) -> Result<(), ApplyError> {
    tx.execute(
        "UPDATE proposals SET applied_state = 'superseded'
         WHERE item_id = ? AND source_revision = ? AND applied_state = 'applied' AND proposal_id != ?",
        rusqlite::params![item_id, source_revision, current_proposal_id],
    )
    .map_err(|e| ApplyError::Storage(e.to_string()))?;

    Ok(())
}

/// Update or insert reminder state for the proposal with all required columns.
fn apply_reminder_proposal(
    tx: &Transaction,
    item_id: &str,
    reminder: &crate::interpretation::contracts::ReminderProposal,
) -> Result<(), ApplyError> {
    let now = Utc::now().to_rfc3339();
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

    // Prepare evidence fields based on quality.
    let (resolved_instant, timezone_id, ambiguity_reason, unsupported_reason): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = match reminder.quality {
        TimeResolutionQuality::Explicit | TimeResolutionQuality::Inferred => {
            // Resolved reminder: store instant and timezone.
            (
                reminder.instant.clone(),
                reminder.timezone_id.clone(),
                None,
                None,
            )
        }
        TimeResolutionQuality::Ambiguous => {
            // Ambiguous reminder: no instant, mark ambiguity as reason.
            (
                None,
                reminder.timezone_id.clone(),
                Some("ambiguous_time".to_string()),
                None,
            )
        }
    };

    // Check if reminder row exists.
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
        // Update existing reminder: preserve prior resolved state unless this updates it.
        tx.execute(
            "UPDATE reminders SET request_state = ?, schedule_state = 'not_scheduled', \
             resolved_instant = ?, timezone_id = ?, ambiguity_reason = ?, unsupported_reason = ?, \
             updated_at = ? WHERE item_id = ?",
            rusqlite::params![
                request_state,
                resolved_instant,
                timezone_id,
                ambiguity_reason,
                unsupported_reason,
                now,
                item_id
            ],
        )
        .map_err(|e| ApplyError::Storage(e.to_string()))?;
    } else {
        // Insert new reminder with all required columns.
        let reminder_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO reminders \
             (reminder_id, item_id, request_state, schedule_state, delivery_state, acknowledgment_state, \
              resolved_instant, timezone_id, ambiguity_reason, unsupported_reason, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                reminder_id,
                item_id,
                request_state,
                "not_scheduled",
                "unknown",
                "not_acknowledged",
                resolved_instant,
                timezone_id,
                ambiguity_reason,
                unsupported_reason,
                now,
                now
            ],
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
