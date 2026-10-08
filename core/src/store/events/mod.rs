// User corrections and lifecycle events with compare-and-set revision semantics.
// Implementation owned by D03.

use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Intent classification of an item. Only an explicit `Action` can carry an obligation;
/// broad intentions, notes and ideas are preserved and searchable but never obligations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    BroadIntention,
    Note,
    Idea,
    Action,
}

impl ItemType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemType::BroadIntention => "broad_intention",
            ItemType::Note => "note",
            ItemType::Idea => "idea",
            ItemType::Action => "action",
        }
    }

    /// Whether an item of this type may carry an obligation (reminder or action suggestion).
    pub fn can_carry_obligation(&self) -> bool {
        matches!(self, ItemType::Action)
    }
}

impl FromStr for ItemType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "broad_intention" => Ok(ItemType::BroadIntention),
            "note" => Ok(ItemType::Note),
            "idea" => Ok(ItemType::Idea),
            "action" => Ok(ItemType::Action),
            _ => Err(anyhow!("Unknown item type: {}", s)),
        }
    }
}

/// Item scope: the privacy classification. Only an explicit user correction changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemScope {
    Personal,
    Work,
}

impl ItemScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemScope::Personal => "personal",
            ItemScope::Work => "work",
        }
    }
}

impl FromStr for ItemScope {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "personal" => Ok(ItemScope::Personal),
            "work" => Ok(ItemScope::Work),
            _ => Err(anyhow!("Unknown item scope: {}", s)),
        }
    }
}

/// Authoritative item state returned with a stale-write rejection so callers can recover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSnapshot {
    pub item_id: String,
    pub revision: i32,
    pub lifecycle_state: String,
    pub item_type: Option<ItemType>,
}

/// Typed outcomes of event persistence.
#[derive(Debug, Error)]
pub enum EventError {
    #[error(
        "stale write on item {item_id}: expected revision {expected}, current revision {}",
        .current.revision
    )]
    StaleRevision {
        item_id: String,
        expected: i32,
        current: ItemSnapshot,
    },
    #[error("event {event_id} already exists with different content")]
    Conflict { event_id: String },
    #[error("event revision {event_revision} does not match expected item revision {expected}")]
    RevisionMismatch { event_revision: i32, expected: i32 },
    #[error("item {item_id} not found")]
    ItemNotFound { item_id: String },
    #[error(
        "{event_type} event not allowed on item {item_id} in lifecycle state '{lifecycle_state}'"
    )]
    NotAllowedInState {
        item_id: String,
        lifecycle_state: String,
        event_type: &'static str,
    },
    #[error(
        "correction of {kind} on item {item_id} states previous value {stated:?} but the current value is {actual:?}"
    )]
    OldValueMismatch {
        item_id: String,
        kind: &'static str,
        stated: Option<String>,
        actual: Option<String>,
    },
    #[error("invalid event: {0}")]
    Invalid(String),
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Database(anyhow::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventType {
    Correction,
    Completion,
    Cancellation,
    SuggestionControl,
    Deletion,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Correction => "correction",
            EventType::Completion => "completion",
            EventType::Cancellation => "cancellation",
            EventType::SuggestionControl => "suggestion_control",
            EventType::Deletion => "deletion",
        }
    }
}

impl FromStr for EventType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "correction" => Ok(EventType::Correction),
            "completion" => Ok(EventType::Completion),
            "cancellation" => Ok(EventType::Cancellation),
            "suggestion_control" => Ok(EventType::SuggestionControl),
            "deletion" => Ok(EventType::Deletion),
            _ => Err(anyhow!("Unknown event type: {}", s)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrectionKind {
    Text,
    Type,
    Scope,
    SessionTopic,
}

impl CorrectionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CorrectionKind::Text => "text",
            CorrectionKind::Type => "type",
            CorrectionKind::Scope => "scope",
            CorrectionKind::SessionTopic => "session_topic",
        }
    }
}

impl FromStr for CorrectionKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "text" => Ok(CorrectionKind::Text),
            "type" => Ok(CorrectionKind::Type),
            "scope" => Ok(CorrectionKind::Scope),
            "session_topic" => Ok(CorrectionKind::SessionTopic),
            _ => Err(anyhow!("Unknown correction kind: {}", s)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuggestionControlKind {
    /// "Not now": cooldown-gated snooze of suggestions for the item.
    NotNow,
    /// "Stop suggesting": pull-only; the item remains searchable.
    StopSuggesting,
}

impl SuggestionControlKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SuggestionControlKind::NotNow => "not_now",
            SuggestionControlKind::StopSuggesting => "stop_suggesting",
        }
    }
}

impl FromStr for SuggestionControlKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "not_now" => Ok(SuggestionControlKind::NotNow),
            "stop_suggesting" => Ok(SuggestionControlKind::StopSuggesting),
            _ => Err(anyhow!("Unknown suggestion control kind: {}", s)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub kind: CorrectionKind,
    pub old_value: Option<String>,
    pub new_value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionControlPayload {
    pub kind: SuggestionControlKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventPayload {
    Correction(Correction),
    SuggestionControl(SuggestionControlPayload),
    Completion,
    Cancellation,
    Deletion,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub event_id: String,
    pub item_id: String,
    pub revision: i32,
    pub event_type: EventType,
    pub payload: EventPayload,
    pub happened_at: String,
}

impl Event {
    pub fn new(
        event_id: String,
        item_id: String,
        revision: i32,
        event_type: EventType,
        payload: EventPayload,
        happened_at: String,
    ) -> Result<Self> {
        if event_id.is_empty() {
            return Err(anyhow!("event_id must not be empty"));
        }
        if item_id.is_empty() {
            return Err(anyhow!("item_id must not be empty"));
        }
        if revision < 0 {
            return Err(anyhow!("revision must be non-negative"));
        }
        let event = Event {
            event_id,
            item_id,
            revision,
            event_type,
            payload,
            happened_at,
        };
        event.validate()?;
        Ok(event)
    }

    /// Reject contradictory type/payload pairs and unsupported values. Fields are public, so
    /// persistence calls this again before any write.
    pub fn validate(&self) -> Result<(), EventError> {
        match (&self.event_type, &self.payload) {
            (EventType::Correction, EventPayload::Correction(correction)) => {
                let invalid = |e: anyhow::Error| EventError::Invalid(e.to_string());
                match correction.kind {
                    CorrectionKind::Type => {
                        correction.new_value.parse::<ItemType>().map_err(invalid)?;
                        if let Some(old_value) = &correction.old_value {
                            old_value.parse::<ItemType>().map_err(invalid)?;
                        }
                    }
                    CorrectionKind::Scope => {
                        correction.new_value.parse::<ItemScope>().map_err(invalid)?;
                        if let Some(old_value) = &correction.old_value {
                            old_value.parse::<ItemScope>().map_err(invalid)?;
                        }
                    }
                    CorrectionKind::SessionTopic => {
                        if correction.new_value.trim().is_empty() {
                            return Err(EventError::Invalid(
                                "session topic must not be empty".to_string(),
                            ));
                        }
                    }
                    CorrectionKind::Text => {}
                }
                Ok(())
            }
            (EventType::SuggestionControl, EventPayload::SuggestionControl(_)) => Ok(()),
            (EventType::Completion, EventPayload::Completion) => Ok(()),
            (EventType::Cancellation, EventPayload::Cancellation) => Ok(()),
            (EventType::Deletion, EventPayload::Deletion) => Ok(()),
            _ => Err(EventError::Invalid(format!(
                "Invalid event type and payload combination: {:?} with {:?}",
                self.event_type, self.payload
            ))),
        }
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.event_id == other.event_id
            && self.item_id == other.item_id
            && self.revision == other.revision
            && self.event_type == other.event_type
            && self.payload == other.payload
            && self.happened_at == other.happened_at
    }
}

impl Eq for Event {}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Event({} on {} at rev {})",
            self.event_id, self.item_id, self.revision
        )
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let event_type_str: String = row.get(3)?;
    let event_type = event_type_str
        .parse::<EventType>()
        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;

    let payload = match event_type {
        EventType::Correction => {
            let kind_str: Option<String> = row.get(5)?;
            let kind = kind_str
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "correction kind must not be null".to_string(),
                    )
                })?
                .parse::<CorrectionKind>()
                .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
            let old_value: Option<String> = row.get(6)?;
            let new_value: String = row.get::<_, Option<String>>(7)?.ok_or_else(|| {
                rusqlite::Error::InvalidParameterName(
                    "correction new_value must not be null".to_string(),
                )
            })?;
            EventPayload::Correction(Correction {
                kind,
                old_value,
                new_value,
            })
        }
        EventType::SuggestionControl => {
            let kind_str: Option<String> = row.get(8)?;
            let kind = kind_str
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "suggestion_control kind must not be null".to_string(),
                    )
                })?
                .parse::<SuggestionControlKind>()
                .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
            EventPayload::SuggestionControl(SuggestionControlPayload { kind })
        }
        EventType::Completion => EventPayload::Completion,
        EventType::Cancellation => EventPayload::Cancellation,
        EventType::Deletion => EventPayload::Deletion,
    };

    Ok(Event {
        event_id: row.get(0)?,
        item_id: row.get(1)?,
        revision: row.get(2)?,
        event_type,
        payload,
        happened_at: row.get(4)?,
    })
}

/// Save an event with compare-and-set revision semantics.
/// Returns the saved event if successful.
/// expected_item_revision: the item's revision at the time of the user's action.
///
/// If an event with the same ID already exists:
/// - Returns the existing event if it is identical (idempotent retry)
/// - Returns `EventError::Conflict` if the content differs
///
/// Compares against the item's authoritative revision; rejects with
/// `EventError::StaleRevision` (carrying the current item state) if expected_item_revision
/// doesn't match the current items.revision. On success, atomically increments items.revision.
pub fn save_event(
    db: &mut Database,
    event: &Event,
    expected_item_revision: i32,
) -> Result<Event, EventError> {
    let tx = db.immediate_transaction().map_err(EventError::Database)?;
    let result = save_event_in_tx(&tx, event, expected_item_revision)?;
    tx.commit()?;
    Ok(result)
}

/// Read the authoritative state of an item, or `None` if it does not exist.
pub fn get_item_snapshot(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<ItemSnapshot>, EventError> {
    let row: Option<(i32, String, Option<String>)> = tx
        .query_row(
            "SELECT revision, lifecycle_state, item_type FROM items WHERE item_id = ?",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(revision, lifecycle_state, item_type)| {
        let item_type = item_type
            .map(|stored| {
                stored
                    .parse::<ItemType>()
                    .map_err(|e| EventError::Corrupt(e.to_string()))
            })
            .transpose()?;
        Ok(ItemSnapshot {
            item_id: item_id.to_string(),
            revision,
            lifecycle_state,
            item_type,
        })
    })
    .transpose()
}

/// Whether the item may carry an obligation. Untyped items and broad intentions, notes and
/// ideas never can; only an item the user (or a validated interpretation) typed as an action can.
pub fn item_can_carry_obligation(tx: &Transaction<'_>, item_id: &str) -> Result<bool, EventError> {
    let snapshot = get_item_snapshot(tx, item_id)?.ok_or_else(|| EventError::ItemNotFound {
        item_id: item_id.to_string(),
    })?;
    Ok(snapshot
        .item_type
        .map(|item_type| item_type.can_carry_obligation())
        .unwrap_or(false))
}

/// The current effective value a correction of `kind` replaces. Scope and session topic fall
/// back to the value recorded on the capture until the first correction; text resolves to the
/// latest text correction, else the original capture text.
fn effective_value(
    tx: &Transaction<'_>,
    item_id: &str,
    kind: &CorrectionKind,
) -> Result<Option<String>, EventError> {
    let sql = match kind {
        CorrectionKind::Type => "SELECT item_type FROM items WHERE item_id = ?1",
        CorrectionKind::Scope => {
            "SELECT COALESCE(i.current_scope, c.item_scope) FROM items i
             JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?1"
        }
        CorrectionKind::SessionTopic => {
            "SELECT COALESCE(i.current_session_topic, c.session_topic) FROM items i
             JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?1"
        }
        CorrectionKind::Text => {
            "SELECT COALESCE(
                 (SELECT new_value FROM corrections
                  WHERE item_id = ?1 AND kind = 'text' ORDER BY revision DESC LIMIT 1),
                 c.text)
             FROM items i JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?1"
        }
    };
    let value: Option<Option<String>> =
        tx.query_row(sql, [item_id], |row| row.get(0)).optional()?;
    Ok(value.flatten())
}

/// Save an event within a caller-owned transaction.
/// expected_item_revision: the item's revision at the time of the user's action.
///
/// Compare-and-set semantics against items.revision: conflicting stale updates are rejected
/// with the current item state returned in `EventError::StaleRevision`.
/// Event.revision must equal expected_item_revision (the authoritative CAS value).
///
/// Never writes reminders or suggestion eligibility: events record user intent only and cannot
/// create obligations.
pub fn save_event_in_tx(
    tx: &Transaction<'_>,
    event: &Event,
    expected_item_revision: i32,
) -> Result<Event, EventError> {
    event.validate()?;

    if event.revision != expected_item_revision {
        return Err(EventError::RevisionMismatch {
            event_revision: event.revision,
            expected: expected_item_revision,
        });
    }

    // Idempotent retry: an identical event already stored has one effect.
    let existing: Option<Event> = tx
        .query_row(
            "SELECT event_id, item_id, revision, event_type, happened_at,
                    correction_kind, correction_old_value, correction_new_value,
                    suggestion_control_kind
             FROM events WHERE event_id = ?",
            [event.event_id.as_str()],
            event_from_row,
        )
        .optional()?;

    if let Some(existing_event) = existing {
        if existing_event == *event {
            return Ok(existing_event);
        }
        return Err(EventError::Conflict {
            event_id: event.event_id.clone(),
        });
    }

    let current =
        get_item_snapshot(tx, &event.item_id)?.ok_or_else(|| EventError::ItemNotFound {
            item_id: event.item_id.clone(),
        })?;

    if expected_item_revision != current.revision {
        return Err(EventError::StaleRevision {
            item_id: event.item_id.clone(),
            expected: expected_item_revision,
            current,
        });
    }

    // Deleted items accept nothing (a racing correction must not restore readable text).
    // Completed and cancelled items stay correctable but take no further lifecycle or
    // suggestion events, except they can be deleted.
    let allowed = match current.lifecycle_state.as_str() {
        "deleted" => false,
        "completed" | "cancelled" => {
            matches!(event.event_type, EventType::Correction | EventType::Deletion)
        }
        _ => true,
    };
    if !allowed {
        return Err(EventError::NotAllowedInState {
            item_id: event.item_id.clone(),
            lifecycle_state: current.lifecycle_state,
            event_type: event.event_type.as_str(),
        });
    }

    if let EventPayload::Correction(corr) = &event.payload {
        let actual = effective_value(tx, &event.item_id, &corr.kind)?;
        if corr.old_value != actual {
            return Err(EventError::OldValueMismatch {
                item_id: event.item_id.clone(),
                kind: corr.kind.as_str(),
                stated: corr.old_value.clone(),
                actual,
            });
        }
    }

    let (correction_kind, correction_old_value, correction_new_value, suggestion_control_kind) =
        match &event.payload {
            EventPayload::Correction(c) => (
                Some(c.kind.as_str()),
                c.old_value.clone(),
                Some(c.new_value.clone()),
                None,
            ),
            EventPayload::SuggestionControl(s) => (None, None, None, Some(s.kind.as_str())),
            EventPayload::Completion => (None, None, None, None),
            EventPayload::Cancellation => (None, None, None, None),
            EventPayload::Deletion => (None, None, None, None),
        };

    tx.execute(
        "INSERT INTO events (event_id, item_id, revision, event_type, happened_at,
                            correction_kind, correction_old_value, correction_new_value,
                            suggestion_control_kind)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &event.event_id,
            &event.item_id,
            event.revision,
            event.event_type.as_str(),
            &event.happened_at,
            correction_kind,
            correction_old_value,
            correction_new_value,
            suggestion_control_kind,
        ],
    )?;

    if let EventPayload::Correction(corr) = &event.payload {
        let correction_id = format!("{}-correction", event.event_id);
        tx.execute(
            "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                &correction_id,
                &event.item_id,
                event.revision,
                corr.kind.as_str(),
                &corr.old_value,
                &corr.new_value,
                &event.happened_at,
            ],
        )?;
        // Type, scope and session-topic corrections define the effective value on the item;
        // the captured source is untouched and text corrections live only in history.
        let effective_column = match corr.kind {
            CorrectionKind::Type => Some("item_type"),
            CorrectionKind::Scope => Some("current_scope"),
            CorrectionKind::SessionTopic => Some("current_session_topic"),
            CorrectionKind::Text => None,
        };
        if let Some(column) = effective_column {
            tx.execute(
                &format!("UPDATE items SET {column} = ? WHERE item_id = ?"),
                rusqlite::params![&corr.new_value, &event.item_id],
            )?;
        }
        if corr.kind == CorrectionKind::Text {
            crate::retrieval::index::sync_item_in_tx(tx, &event.item_id)
                .map_err(EventError::Database)?;
        }
    }

    match event.event_type {
        EventType::Completion => {
            tx.execute(
                "UPDATE items SET lifecycle_state = 'completed' WHERE item_id = ?",
                [event.item_id.as_str()],
            )?;
        }
        EventType::Cancellation => {
            tx.execute(
                "UPDATE items SET lifecycle_state = 'cancelled' WHERE item_id = ?",
                [event.item_id.as_str()],
            )?;
        }
        EventType::Deletion => {
            tx.execute(
                "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
                [event.item_id.as_str()],
            )?;
        }
        _ => {}
    }

    tx.execute(
        "UPDATE items SET revision = revision + 1 WHERE item_id = ?",
        [event.item_id.as_str()],
    )?;

    Ok(event.clone())
}

/// Retrieve an event by ID.
pub fn get_event(tx: &Transaction<'_>, event_id: &str) -> Result<Option<Event>> {
    tx.query_row(
        "SELECT event_id, item_id, revision, event_type, happened_at,
                correction_kind, correction_old_value, correction_new_value,
                suggestion_control_kind
         FROM events WHERE event_id = ?",
        [event_id],
        event_from_row,
    )
    .optional()
    .map_err(|e| anyhow!(e))
}

/// Retrieve all events for an item, ordered by revision ascending.
pub fn get_events_for_item(tx: &Transaction<'_>, item_id: &str) -> Result<Vec<Event>> {
    let mut stmt = tx.prepare(
        "SELECT event_id, item_id, revision, event_type, happened_at,
                correction_kind, correction_old_value, correction_new_value,
                suggestion_control_kind
         FROM events
         WHERE item_id = ? ORDER BY revision ASC",
    )?;

    let events = stmt
        .query_map([item_id], event_from_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(events)
}

/// Delete all events for an item.
pub fn delete_events_for_item(tx: &Transaction<'_>, item_id: &str) -> Result<usize> {
    tx.execute("DELETE FROM corrections WHERE item_id = ?", [item_id])?;
    let deleted = tx
        .execute("DELETE FROM events WHERE item_id = ?", [item_id])
        .map_err(|e| anyhow!(e))?;
    crate::retrieval::index::sync_item_in_tx(tx, item_id)?;
    Ok(deleted)
}
