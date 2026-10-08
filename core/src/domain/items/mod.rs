// Authoritative item state projection from sources and explicit updates.
// Precedence: source capture record, then explicit user corrections, then model annotations.
// Model reprocessing cannot undo corrections, completion, or cancellation.
// Implementation owned by D04.

// Re-export types from events module for convenience in D04 tests.
pub use crate::store::events::{ItemScope, ItemType};
use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;

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
    /// Untyped items cannot carry reminders. Only user corrections (not model) define type.
    pub item_type: Option<ItemType>,
    /// Privacy scope: personal or work. User corrections override the capture-time default.
    pub scope: ItemScope,
    /// Optional user-defined session topic. Updated only by explicit correction.
    pub session_topic: Option<String>,
    /// Lifecycle: active -> completed/cancelled -> deleted (irreversible).
    pub lifecycle_state: LifecycleState,
    /// Text state: the current effective text (capture or latest user text correction).
    pub current_text: TextState,
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

        StateTransition::ModelAnnotation { .. } => {
            // Model output cannot override corrections, completion or cancellation.
            // Only Active items (and only if not yet corrected/completed/cancelled) can accept
            // model type suggestions.
            match current.lifecycle_state {
                LifecycleState::Completed | LifecycleState::Cancelled | LifecycleState::Deleted => {
                    Ok(ForbiddenOverride)
                }
                LifecycleState::Active => Ok(Valid),
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

    // Resolve session topic: user correction takes precedence, else capture default.
    let session_topic = current_session_topic.or(capture_session_topic);

    // Resolve text: check for user text correction.
    let current_text = {
        let text_correction: Option<(String, String)> = tx
            .query_row(
                "SELECT new_value, created_at FROM corrections
                 WHERE item_id = ? AND kind = 'text' ORDER BY revision DESC LIMIT 1",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match text_correction {
            Some((corrected_text, corrected_at)) => TextState::Corrected {
                text: corrected_text,
                corrected_at,
            },
            None => TextState::Original { text: capture_text },
        }
    };

    Ok(Some(ItemState {
        item_id: item_id.to_string(),
        capture_id,
        revision,
        item_type: parsed_item_type,
        scope,
        session_topic,
        lifecycle_state,
        current_text,
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
    let mut current_type: Option<ItemType> = None;
    let mut current_scope = capture_scope;
    let mut current_session_topic = capture_session_topic;
    let mut current_text = TextState::Original {
        text: capture_text.clone(),
    };
    let mut lifecycle_state = LifecycleState::Active;

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
            }
            "scope" => {
                current_scope = new_value
                    .parse::<ItemScope>()
                    .map_err(|e| anyhow!("Invalid corrected scope: {}", e))?;
            }
            "session_topic" => {
                current_session_topic = Some(new_value);
            }
            "text" => {
                current_text = TextState::Corrected {
                    text: new_value,
                    corrected_at,
                };
            }
            _ => {} // ignore unknown correction types
        }
    }

    // Replay all lifecycle events in order.
    let mut events_stmt =
        tx.prepare("SELECT event_type FROM events WHERE item_id = ? ORDER BY revision ASC")?;
    let events = events_stmt
        .query_map([item_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

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

    // Query final revision from items table.
    let revision: i32 = tx.query_row(
        "SELECT revision FROM items WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;

    Ok(Some(ItemState {
        item_id: item_id.to_string(),
        capture_id,
        revision,
        item_type: current_type,
        scope: current_scope,
        session_topic: current_session_topic,
        lifecycle_state,
        current_text,
    }))
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
}
