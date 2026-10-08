// Authoritative item state projection from sources and explicit updates.
// Precedence: source capture record, then explicit user corrections, then model annotations.
// Model reprocessing cannot undo corrections, completion, or cancellation.
// Implementation owned by D04.

// Re-export types from events module for convenience in D04 tests.
pub use crate::store::events::{ItemScope, ItemType};
use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;

/// Field provenance: whether a field was set by the user (correction) or is derived/default.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FieldProvenance {
    /// True if item_type was explicitly corrected by user.
    pub type_corrected: bool,
    /// True if scope was explicitly corrected by user.
    pub scope_corrected: bool,
    /// True if session_topic was explicitly corrected by user.
    pub session_topic_corrected: bool,
    /// True if text was explicitly corrected by user.
    pub text_corrected: bool,
}

/// Current projected state of an item, derived from capture and all authoritative updates.
/// Reflects the "read what the application sees now" perspective, not what a model most recently proposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemState {
    /// Unique identifier from the source capture.
    pub item_id: String,
    /// Stable reference back to the originating capture (never changes).
    pub capture_id: String,
    /// Current versioning epoch for conflict detection and correction tracking.
    pub revision: i32,
    /// Classification: action (obligatory), idea, note, or broad intention (non-obligatory).
    /// Untyped items cannot carry reminders. Can be set by user correction or applied model proposal.
    pub item_type: Option<ItemType>,
    /// Privacy scope: personal or work. User corrections override the capture-time default.
    pub scope: ItemScope,
    /// Optional user-defined session topic. Updated only by explicit correction.
    pub session_topic: Option<String>,
    /// Lifecycle: active -> completed/cancelled -> deleted (irreversible).
    pub lifecycle_state: LifecycleState,
    /// Text state: the current effective text (capture or latest user text correction).
    pub current_text: TextState,
    /// Per-field provenance: tracks which fields were set by user corrections.
    pub provenance: FieldProvenance,
}

/// Lifecycle progression of an item. Once deleted, the item is readonly and no further
/// mutations are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleState {
    Active,
    Completed,
    Cancelled,
    Deleted,
}

impl LifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            LifecycleState::Active => "active",
            LifecycleState::Completed => "completed",
            LifecycleState::Cancelled => "cancelled",
            LifecycleState::Deleted => "deleted",
        }
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Current text state: either the original capture text or a user correction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextState {
    /// Original text from capture (or empty if only audio).
    Original { text: Option<String> },
    /// Corrected by the user; preserves the timestamp of the correction event.
    Corrected { text: String, corrected_at: String },
}

impl TextState {
    pub fn text(&self) -> Option<&str> {
        match self {
            TextState::Original { text } => text.as_deref(),
            TextState::Corrected { text, .. } => Some(text),
        }
    }
}

/// Outcome of validating a state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionValidity {
    /// The transition is allowed.
    Valid,
    /// The transition is forbidden: model/system output tried to override an authoritative state.
    ForbiddenOverride,
    /// The transition is not allowed from this lifecycle state.
    NotAllowed,
}

/// Check whether a transition from current to proposed state is valid.
/// Returns `Ok(TransitionValidity::Valid)` if allowed, or describes why it is forbidden.
pub fn validate_state_transition(
    current: &ItemState,
    proposed: StateTransition,
) -> Result<TransitionValidity, StateTransitionError> {
    use TransitionValidity::*;

    // Deleted items accept no mutations.
    if current.lifecycle_state == LifecycleState::Deleted {
        return Ok(NotAllowed);
    }

    match proposed {
        StateTransition::TypeSet(_) => {
            // Type changes are allowed at any lifecycle state (even completed/cancelled).
            Ok(Valid)
        }

        StateTransition::ScopeSet(_) => {
            // Scope changes are allowed at any lifecycle state.
            Ok(Valid)
        }

        StateTransition::SessionTopicSet(_) => {
            // Session topic changes are allowed at any lifecycle state.
            Ok(Valid)
        }

        StateTransition::TextCorrected(_) => {
            // Text corrections are allowed at any lifecycle state (even completed/cancelled).
            Ok(Valid)
        }

        StateTransition::Completed => {
            match current.lifecycle_state {
                LifecycleState::Active => Ok(Valid),
                LifecycleState::Completed => Ok(Valid), // idempotent
                LifecycleState::Cancelled | LifecycleState::Deleted => Ok(NotAllowed),
            }
        }

        StateTransition::Cancelled => {
            match current.lifecycle_state {
                LifecycleState::Active => Ok(Valid),
                LifecycleState::Cancelled => Ok(Valid), // idempotent
                LifecycleState::Completed | LifecycleState::Deleted => Ok(NotAllowed),
            }
        }

        StateTransition::Deleted => {
            match current.lifecycle_state {
                LifecycleState::Active | LifecycleState::Completed | LifecycleState::Cancelled => {
                    Ok(Valid)
                }
                LifecycleState::Deleted => Ok(Valid), // idempotent
            }
        }

        StateTransition::ModelAnnotation {
            annotation_type: model_type,
        } => {
            // Model output cannot override corrections, completion or cancellation.
            // Check lifecycle first.
            match current.lifecycle_state {
                LifecycleState::Completed | LifecycleState::Cancelled | LifecycleState::Deleted => {
                    return Ok(ForbiddenOverride);
                }
                LifecycleState::Active => {}
            }
            // On active items, model can only set type if not already user-corrected.
            if model_type.is_some() && current.provenance.type_corrected {
                Ok(ForbiddenOverride)
            } else {
                Ok(Valid)
            }
        }
    }
}

/// An explicit state transition requested by user action or authoritative system change.
#[derive(Clone, Debug)]
pub enum StateTransition {
    /// Explicit type setting (user or initial interpretation).
    TypeSet(Option<ItemType>),
    /// Explicit scope setting.
    ScopeSet(ItemScope),
    /// Explicit session topic setting.
    SessionTopicSet(Option<String>),
    /// User-corrected text.
    TextCorrected(String),
    /// User marked item as completed.
    Completed,
    /// User marked item as cancelled.
    Cancelled,
    /// System marked item as deleted.
    Deleted,
    /// Model provided annotation (type suggestion, etc). Can be rejected if current state forbids it.
    ModelAnnotation { annotation_type: Option<ItemType> },
}

#[derive(Clone, Debug)]
pub enum StateTransitionError {
    InvalidProposedValue(String),
}

impl fmt::Display for StateTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateTransitionError::InvalidProposedValue(msg) => write!(f, "Invalid value: {}", msg),
        }
    }
}

impl std::error::Error for StateTransitionError {}

/// Load the current authoritative state of an item from the database.
/// Returns None if the item does not exist.
/// Rebuilds state from: capture (source of truth), item record (current values and lifecycle),
/// corrections (user updates), and events (lifecycle changes).
pub fn load_item_state(tx: &Transaction<'_>, item_id: &str) -> Result<Option<ItemState>> {
    type ItemRow = (
        String,
        i32,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    // Fetch the item record; if missing, return None.
    let item_row: Option<ItemRow> =
        tx.query_row(
            "SELECT capture_id, revision, item_type, current_scope, current_session_topic, lifecycle_state
             FROM items WHERE item_id = ?",
            [item_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;

    let (capture_id, revision, item_type, current_scope, current_session_topic, lifecycle_str) =
        match item_row {
            Some(row) => row,
            None => return Ok(None),
        };

    // Fetch the capture record (authoritative source).
    let (capture_text, capture_scope, capture_session_topic): (
        Option<String>,
        String,
        Option<String>,
    ) = tx.query_row(
        "SELECT text, item_scope, session_topic FROM captures WHERE capture_id = ?",
        [&capture_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    // Resolve lifecycle state.
    let lifecycle_state = match lifecycle_str.as_str() {
        "active" => LifecycleState::Active,
        "completed" => LifecycleState::Completed,
        "cancelled" => LifecycleState::Cancelled,
        "deleted" => LifecycleState::Deleted,
        _ => return Err(anyhow!("Unknown lifecycle state: {}", lifecycle_str)),
    };

    // Resolve item type.
    let parsed_item_type = item_type
        .as_ref()
        .map(|s| s.parse::<ItemType>())
        .transpose()
        .map_err(|e| anyhow!("Invalid item type: {}", e))?;

    // Track provenance: which fields have user corrections (from corrections table, not from columns).
    let type_corrected = tx.query_row(
        "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'type'",
        [item_id],
        |row| row.get::<_, i64>(0),
    )? > 0;
    let scope_corrected = tx.query_row(
        "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'scope'",
        [item_id],
        |row| row.get::<_, i64>(0),
    )? > 0;
    let session_topic_corrected = tx.query_row(
        "SELECT COUNT(*) FROM corrections WHERE item_id = ? AND kind = 'session_topic'",
        [item_id],
        |row| row.get::<_, i64>(0),
    )? > 0;

    // Resolve scope: user correction takes precedence, else capture default.
    let scope: ItemScope = if let Some(scope_str) = current_scope {
        scope_str
            .parse::<ItemScope>()
            .map_err(|e| anyhow!("Invalid scope: {}", e))?
    } else {
        capture_scope
            .parse::<ItemScope>()
            .map_err(|e| anyhow!("Invalid capture scope: {}", e))?
    };

    // Resolve session topic: user correction takes precedence, else capture default, else applied proposal.
    // Check if capture has an explicit session_topic to enforce its precedence over proposals.
    let has_capture_topic = capture_session_topic.is_some();
    let mut session_topic = current_session_topic.or(capture_session_topic);

    // Resolve item type: user correction takes precedence over applied proposals.
    let mut resolved_type = parsed_item_type;
    if !type_corrected {
        // Check for applied proposals with type (ordered by source_revision for determinism).
        let applied_type: Option<String> = tx
            .query_row(
                "SELECT proposal_type FROM proposals
                 WHERE item_id = ? AND applied_state = 'applied' AND proposal_type IS NOT NULL AND abstained = 0 AND schema_version IS NOT NULL
                 ORDER BY source_revision DESC LIMIT 1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(type_str) = applied_type {
            resolved_type = Some(
                type_str
                    .parse::<ItemType>()
                    .map_err(|e| anyhow!("Invalid applied proposal type: {}", e))?,
            );
        }
    }

    // Similarly, check for applied session_topic if not user-corrected.
    // But never override an explicit session_topic from the capture itself.
    if !session_topic_corrected && !has_capture_topic {
        let applied_topic: Option<String> = tx
            .query_row(
                "SELECT session_topic_proposal FROM proposals
                 WHERE item_id = ? AND applied_state = 'applied' AND session_topic_proposal IS NOT NULL AND abstained = 0 AND schema_version IS NOT NULL
                 ORDER BY source_revision DESC LIMIT 1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(topic) = applied_topic {
            session_topic = Some(topic);
        }
    }

    // Resolve text: check for user text correction.
    let (current_text, text_corrected) = {
        let text_correction: Option<(String, String)> = tx
            .query_row(
                "SELECT new_value, created_at FROM corrections
                 WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match text_correction {
            Some((corrected_text, corrected_at)) => (
                TextState::Corrected {
                    text: corrected_text,
                    corrected_at,
                },
                true,
            ),
            None => (TextState::Original { text: capture_text }, false),
        }
    };

    Ok(Some(ItemState {
        item_id: item_id.to_string(),
        capture_id,
        revision,
        item_type: resolved_type,
        scope,
        session_topic,
        lifecycle_state,
        current_text,
        provenance: FieldProvenance {
            type_corrected,
            scope_corrected,
            session_topic_corrected,
            text_corrected,
        },
    }))
}

/// Verify that the state snapshot stored in the database can be rebuilt from retained records.
/// If the stored snapshot differs from the rebuilt state, returns the discrepancy.
/// This is used to detect stale or corrupted derived output.
pub fn verify_state_integrity(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<StateIntegrityIssue>> {
    let stored = load_item_state(tx, item_id)?;
    let rebuilt = rebuild_state_from_events(tx, item_id)?;

    match (stored, rebuilt) {
        (None, None) => Ok(None), // both missing is consistent
        (None, Some(_)) => Ok(Some(StateIntegrityIssue::ExtraRebuiltState)),
        (Some(_), None) => Ok(Some(StateIntegrityIssue::MissingRebuiltState)),
        (Some(stored_state), Some(rebuilt_state)) => {
            if stored_state == rebuilt_state {
                Ok(None)
            } else {
                Ok(Some(StateIntegrityIssue::Mismatch {
                    stored: Box::new(stored_state),
                    rebuilt: Box::new(rebuilt_state),
                }))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateIntegrityIssue {
    /// The database has a stored item but it cannot be rebuilt from events.
    MissingRebuiltState,
    /// Events exist but the stored item is missing.
    ExtraRebuiltState,
    /// Stored and rebuilt states differ.
    Mismatch {
        stored: Box<ItemState>,
        rebuilt: Box<ItemState>,
    },
}

/// Rebuild an item's state from scratch by replaying all retained records (capture, corrections, events).
/// Used to verify integrity or recover state after suspected corruption.
/// Returns None if the item has no corresponding capture.
pub fn rebuild_state_from_events(tx: &Transaction<'_>, item_id: &str) -> Result<Option<ItemState>> {
    // Start with the capture record as the source of truth.
    type CaptureRow = (String, Option<String>, String, Option<String>);
    let capture_row: Option<CaptureRow> = tx
        .query_row(
            "SELECT c.capture_id, c.text, c.item_scope, c.session_topic FROM captures c
             JOIN items i ON c.capture_id = i.capture_id WHERE i.item_id = ?",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let (capture_id, capture_text, capture_scope_str, capture_session_topic) = match capture_row {
        Some(row) => row,
        None => return Ok(None),
    };

    let capture_scope: ItemScope = capture_scope_str
        .parse::<ItemScope>()
        .map_err(|e| anyhow!("Invalid capture scope: {}", e))?;

    // Start with defaults from capture.
    let has_capture_topic = capture_session_topic.is_some();
    let mut current_type: Option<ItemType> = None;
    let mut current_scope = capture_scope;
    let mut current_session_topic = capture_session_topic.clone();
    let mut current_text = TextState::Original {
        text: capture_text.clone(),
    };
    let mut lifecycle_state = LifecycleState::Active;

    // Track provenance: which fields came from user corrections vs model proposals.
    let mut type_corrected = false;
    let mut scope_corrected = false;
    let mut session_topic_corrected = false;
    let mut text_corrected = false;

    // Replay all corrections in order.
    let mut corrections_stmt = tx.prepare(
        "SELECT kind, new_value, created_at FROM corrections WHERE item_id = ? ORDER BY revision ASC",
    )?;
    let corrections = corrections_stmt
        .query_map([item_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (kind, new_value, corrected_at) in corrections {
        match kind.as_str() {
            "type" => {
                current_type = Some(
                    new_value
                        .parse::<ItemType>()
                        .map_err(|e| anyhow!("Invalid corrected type: {}", e))?,
                );
                type_corrected = true;
            }
            "scope" => {
                current_scope = new_value
                    .parse::<ItemScope>()
                    .map_err(|e| anyhow!("Invalid corrected scope: {}", e))?;
                scope_corrected = true;
            }
            "session_topic" => {
                current_session_topic = Some(new_value);
                session_topic_corrected = true;
            }
            "text" => {
                current_text = TextState::Corrected {
                    text: new_value,
                    corrected_at,
                };
                text_corrected = true;
            }
            _ => {} // ignore unknown correction types
        }
    }

    // Replay applied proposals with lower precedence than user corrections.
    // Precedence: capture (source) < applied model proposals < user corrections.
    // Order by source_revision for determinism (last one wins).
    let mut applied_proposals_stmt = tx.prepare(
        "SELECT proposal_type, session_topic_proposal FROM proposals
         WHERE item_id = ? AND applied_state = 'applied' AND abstained = 0 AND schema_version IS NOT NULL
         ORDER BY source_revision ASC",
    )?;
    let applied_proposals = applied_proposals_stmt
        .query_map([item_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (proposal_type, session_topic_proposal) in applied_proposals {
        // Only apply if the user hasn't corrected the field.
        if let Some(ptype) = proposal_type {
            if !type_corrected {
                current_type = ptype.parse::<ItemType>().ok();
            }
        }

        // Never override an explicit session_topic from the capture itself.
        if let Some(session_topic) = session_topic_proposal {
            if !session_topic_corrected && !has_capture_topic {
                current_session_topic = Some(session_topic);
            }
        }
    }

    // Replay all lifecycle events in order.
    let mut events_stmt =
        tx.prepare("SELECT event_type FROM events WHERE item_id = ? ORDER BY revision ASC")?;
    let events = events_stmt
        .query_map([item_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let num_events = events.len();
    for event_type in events {
        match event_type.as_str() {
            "completion" => {
                lifecycle_state = LifecycleState::Completed;
            }
            "cancellation" => {
                lifecycle_state = LifecycleState::Cancelled;
            }
            "deletion" => {
                lifecycle_state = LifecycleState::Deleted;
            }
            _ => {} // ignore unknown event types (corrections, suggestion_control don't change lifecycle)
        }
    }

    // Rebuild revision from the event count, not from items table.
    // Each saved event increments revision by 1, so revision equals the number of events.
    let revision = num_events as i32;

    Ok(Some(ItemState {
        item_id: item_id.to_string(),
        capture_id,
        revision,
        item_type: current_type,
        scope: current_scope,
        session_topic: current_session_topic,
        lifecycle_state,
        current_text,
        provenance: FieldProvenance {
            type_corrected,
            scope_corrected,
            session_topic_corrected,
            text_corrected,
        },
    }))
}

/// Error returned by guarded proposal application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalApplicationError {
    /// The proposal's source_revision does not match the item's current revision.
    StaleProposal {
        source_revision: i32,
        current_revision: i32,
    },
    /// The proposal has already been applied.
    AlreadyApplied,
    /// The item state forbids applying this proposal (e.g., completed/cancelled/deleted).
    ForbiddenByLifecycle,
    /// A field the proposal tries to set was explicitly corrected by the user.
    ForbiddenByUserCorrection(String),
    /// Database error or validation failure.
    InvalidProposal(String),
}

impl fmt::Display for ProposalApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposalApplicationError::StaleProposal {
                source_revision,
                current_revision,
            } => write!(
                f,
                "Proposal is stale: source_revision {} != current_revision {}",
                source_revision, current_revision
            ),
            ProposalApplicationError::AlreadyApplied => {
                write!(f, "Proposal has already been applied")
            }
            ProposalApplicationError::ForbiddenByLifecycle => {
                write!(f, "Item lifecycle state forbids applying this proposal")
            }
            ProposalApplicationError::ForbiddenByUserCorrection(field) => {
                write!(
                    f,
                    "Field '{}' was explicitly corrected by the user; model cannot override",
                    field
                )
            }
            ProposalApplicationError::InvalidProposal(msg) => {
                write!(f, "Invalid proposal: {}", msg)
            }
        }
    }
}

impl std::error::Error for ProposalApplicationError {}

/// Apply a model proposal to an item, guarded against stale/invalid output.
/// Returns the updated ItemState if successful, or a ProposalApplicationError.
/// The proposal must have its stored source_revision matching current item revision.
/// Stale or invalid proposals preserve the prior state rather than demoting the item.
/// Successfully applied values are persisted to the authoritative items table.
pub fn apply_proposal(
    tx: &Transaction<'_>,
    item_id: &str,
    proposal_id: &str,
) -> Result<ItemState, ProposalApplicationError> {
    // Load current state to check conditions.
    let current = load_item_state(tx, item_id)
        .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?
        .ok_or(ProposalApplicationError::InvalidProposal(
            "Item not found".to_string(),
        ))?;

    // Fetch the proposal row, filtering by both proposal_id AND item_id (ownership check).
    #[allow(clippy::type_complexity)]
    let proposal_row: Option<(i32, Option<i32>, Option<String>, Option<String>, i64, String)> = tx
        .query_row(
            "SELECT source_revision, schema_version, proposal_type, session_topic_proposal, abstained, applied_state FROM proposals
             WHERE proposal_id = ? AND item_id = ? AND capture_id = ?",
            rusqlite::params![proposal_id, item_id, &current.capture_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?;

    let (
        stored_source_revision,
        schema_version,
        proposal_type,
        session_topic_proposal,
        abstained,
        applied_state,
    ) = proposal_row.ok_or(ProposalApplicationError::InvalidProposal(
        "Proposal not found or belongs to another item".to_string(),
    ))?;

    // Check staleness using stored source_revision.
    if stored_source_revision != current.revision {
        return Err(ProposalApplicationError::StaleProposal {
            source_revision: stored_source_revision,
            current_revision: current.revision,
        });
    }

    // Check that the proposal has not already been applied.
    if applied_state == "applied" {
        return Err(ProposalApplicationError::AlreadyApplied);
    }

    // Check that the proposal is not abstained.
    if abstained != 0 {
        return Err(ProposalApplicationError::InvalidProposal(
            "Proposal is abstained".to_string(),
        ));
    }

    // Validate schema_version is present.
    if schema_version.is_none() {
        return Err(ProposalApplicationError::InvalidProposal(
            "Proposal has no schema_version".to_string(),
        ));
    }

    // Check that the item lifecycle permits application (completed/cancelled/deleted forbid model updates).
    if current.lifecycle_state != LifecycleState::Active {
        return Err(ProposalApplicationError::ForbiddenByLifecycle);
    }

    // Validate and prepare the proposed type (if present and not user-corrected).
    let mut new_item_type = current.item_type;
    if let Some(ptype) = &proposal_type {
        if current.provenance.type_corrected {
            return Err(ProposalApplicationError::ForbiddenByUserCorrection(
                "type".to_string(),
            ));
        }
        // Validate that the proposed type is a valid ItemType value.
        new_item_type = Some(ptype.parse::<ItemType>().map_err(|e| {
            ProposalApplicationError::InvalidProposal(format!("Invalid proposal_type: {}", e))
        })?);
    }

    // Validate and prepare the proposed session_topic (if present and not user-corrected).
    let mut new_session_topic = current.session_topic.clone();
    if let Some(topic) = &session_topic_proposal {
        if current.provenance.session_topic_corrected {
            return Err(ProposalApplicationError::ForbiddenByUserCorrection(
                "session_topic".to_string(),
            ));
        }
        // Also check that the capture didn't have an explicit session_topic.
        // If it did, the proposal should not override it.
        let capture_topic: Option<String> = tx
            .query_row(
                "SELECT session_topic FROM captures WHERE capture_id = ?",
                [&current.capture_id],
                |row| row.get(0),
            )
            .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?;

        if capture_topic.is_some() {
            // Capture has an explicit session_topic; don't override it.
            // This is not an error; just preserve the current state.
            new_session_topic = current.session_topic.clone();
        } else {
            new_session_topic = Some(topic.clone());
        }
    }

    // Mark proposal as applied and update the item's authoritative state in a single transaction.
    tx.execute(
        "UPDATE proposals SET applied_state = 'applied' WHERE proposal_id = ?",
        [proposal_id],
    )
    .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?;

    // Update the items table with the new type and session_topic.
    // Do not bump revision; revision is derived from events and corrections only.
    let new_item_type_str = new_item_type.as_ref().map(|t| t.as_str());
    tx.execute(
        "UPDATE items SET item_type = ?, current_session_topic = ? WHERE item_id = ?",
        rusqlite::params![new_item_type_str, new_session_topic, item_id],
    )
    .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?;

    // Rebuild the state to include the newly applied proposal.
    let updated_state = rebuild_state_from_events(tx, item_id)
        .map_err(|e| ProposalApplicationError::InvalidProposal(e.to_string()))?
        .ok_or(ProposalApplicationError::InvalidProposal(
            "Failed to rebuild item state after proposal application".to_string(),
        ))?;

    Ok(updated_state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_state_ordering() {
        assert!(LifecycleState::Active < LifecycleState::Completed);
        assert!(LifecycleState::Completed < LifecycleState::Cancelled);
        assert!(LifecycleState::Cancelled < LifecycleState::Deleted);
    }

    #[test]
    fn test_text_state_extraction() {
        let original = TextState::Original {
            text: Some("hello".to_string()),
        };
        assert_eq!(original.text(), Some("hello"));

        let corrected = TextState::Corrected {
            text: "world".to_string(),
            corrected_at: "2026-01-15T10:30:00Z".to_string(),
        };
        assert_eq!(corrected.text(), Some("world"));

        let empty_original = TextState::Original { text: None };
        assert_eq!(empty_original.text(), None);
    }

    #[test]
    fn test_field_provenance_defaults() {
        let provenance = FieldProvenance::default();
        assert!(!provenance.type_corrected);
        assert!(!provenance.scope_corrected);
        assert!(!provenance.session_topic_corrected);
        assert!(!provenance.text_corrected);
    }

    #[test]
    fn test_transition_completable_only_when_active() -> Result<(), StateTransitionError> {
        let mut state = ItemState {
            item_id: "test".to_string(),
            capture_id: "cap".to_string(),
            revision: 0,
            item_type: Some(ItemType::Action),
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Active,
            current_text: TextState::Original {
                text: Some("test".to_string()),
            },
            provenance: FieldProvenance::default(),
        };

        // Active -> Completed is valid.
        assert_eq!(
            validate_state_transition(&state, StateTransition::Completed)?,
            TransitionValidity::Valid
        );

        // Mark as completed.
        state.lifecycle_state = LifecycleState::Completed;

        // Completed -> Cancelled is not allowed.
        assert_eq!(
            validate_state_transition(&state, StateTransition::Cancelled)?,
            TransitionValidity::NotAllowed
        );

        // But Completed -> Completed (idempotent) is valid.
        assert_eq!(
            validate_state_transition(&state, StateTransition::Completed)?,
            TransitionValidity::Valid
        );

        Ok(())
    }

    #[test]
    fn test_model_cannot_override_completed() -> Result<(), StateTransitionError> {
        let state = ItemState {
            item_id: "test".to_string(),
            capture_id: "cap".to_string(),
            revision: 0,
            item_type: None,
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Completed,
            current_text: TextState::Original {
                text: Some("test".to_string()),
            },
            provenance: FieldProvenance::default(),
        };

        // Model trying to set type on a completed item is forbidden.
        assert_eq!(
            validate_state_transition(
                &state,
                StateTransition::ModelAnnotation {
                    annotation_type: Some(ItemType::Action)
                }
            )?,
            TransitionValidity::ForbiddenOverride
        );

        Ok(())
    }

    #[test]
    fn test_deleted_item_readonly() -> Result<(), StateTransitionError> {
        let state = ItemState {
            item_id: "test".to_string(),
            capture_id: "cap".to_string(),
            revision: 0,
            item_type: Some(ItemType::Action),
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Deleted,
            current_text: TextState::Original {
                text: Some("test".to_string()),
            },
            provenance: FieldProvenance::default(),
        };

        // Any transition from deleted should be rejected as NotAllowed.
        assert_eq!(
            validate_state_transition(&state, StateTransition::Completed)?,
            TransitionValidity::NotAllowed
        );

        assert_eq!(
            validate_state_transition(&state, StateTransition::TypeSet(Some(ItemType::Idea)))?,
            TransitionValidity::NotAllowed
        );

        Ok(())
    }

    #[test]
    fn test_corrections_allowed_on_completed() -> Result<(), StateTransitionError> {
        let state = ItemState {
            item_id: "test".to_string(),
            capture_id: "cap".to_string(),
            revision: 0,
            item_type: Some(ItemType::Action),
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Completed,
            current_text: TextState::Original {
                text: Some("test".to_string()),
            },
            provenance: FieldProvenance::default(),
        };

        // User corrections are still allowed on completed items.
        assert_eq!(
            validate_state_transition(
                &state,
                StateTransition::TextCorrected("updated".to_string())
            )?,
            TransitionValidity::Valid
        );

        assert_eq!(
            validate_state_transition(&state, StateTransition::ScopeSet(ItemScope::Work))?,
            TransitionValidity::Valid
        );

        Ok(())
    }

    #[test]
    fn test_model_cannot_override_user_corrected_type() -> Result<(), StateTransitionError> {
        let state = ItemState {
            item_id: "test".to_string(),
            capture_id: "cap".to_string(),
            revision: 2,
            item_type: Some(ItemType::Action),
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Active,
            current_text: TextState::Original {
                text: Some("test".to_string()),
            },
            provenance: FieldProvenance {
                type_corrected: true,
                scope_corrected: false,
                session_topic_corrected: false,
                text_corrected: false,
            },
        };

        // Model tries to set type on an item where user already corrected it.
        assert_eq!(
            validate_state_transition(
                &state,
                StateTransition::ModelAnnotation {
                    annotation_type: Some(ItemType::Idea)
                }
            )?,
            TransitionValidity::ForbiddenOverride
        );

        Ok(())
    }
}
