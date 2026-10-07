// User corrections and lifecycle events with compare-and-set revision semantics.
// Implementation owned by D03.

use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventType {
    Correction,
    Completion,
    Cancellation,
    SuggestionControl,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Correction => "correction",
            EventType::Completion => "completion",
            EventType::Cancellation => "cancellation",
            EventType::SuggestionControl => "suggestion_control",
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
            _ => Err(anyhow!("Unknown event type: {}", s)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub kind: String,
    pub old_value: Option<String>,
    pub new_value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionControlPayload {
    pub kind: String, // "not_now", "cooldown", "stop_suggesting"
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventPayload {
    Correction(Correction),
    SuggestionControl(SuggestionControlPayload),
    Completion,
    Cancellation,
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
        Ok(Event {
            event_id,
            item_id,
            revision,
            event_type,
            payload,
            happened_at,
        })
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
            let kind: Option<String> = row.get(5)?;
            let old_value: Option<String> = row.get(6)?;
            let new_value: Option<String> = row.get(7)?;
            EventPayload::Correction(Correction {
                kind: kind.unwrap_or_default(),
                old_value,
                new_value: new_value.unwrap_or_default(),
            })
        }
        EventType::SuggestionControl => {
            let kind: Option<String> = row.get(8)?;
            EventPayload::SuggestionControl(SuggestionControlPayload {
                kind: kind.unwrap_or_default(),
            })
        }
        EventType::Completion => EventPayload::Completion,
        EventType::Cancellation => EventPayload::Cancellation,
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
/// - Returns an error if it conflicts
///
/// Compares against the item's authoritative revision; rejects if expected_item_revision
/// doesn't match the current items.revision. On success, atomically increments items.revision.
pub fn save_event(db: &mut Database, event: &Event, expected_item_revision: i32) -> Result<Event> {
    let tx = db.immediate_transaction()?;
    let result = save_event_in_tx(&tx, event, expected_item_revision)?;
    tx.commit()?;
    Ok(result)
}

/// Save an event within a caller-owned transaction.
/// expected_item_revision: the item's revision at the time of the user's action.
///
/// Compare-and-set semantics against items.revision: conflicting stale updates are rejected
/// with the current item state returned in the error.
pub fn save_event_in_tx(
    tx: &Transaction<'_>,
    event: &Event,
    expected_item_revision: i32,
) -> Result<Event> {
    // Check if event with same ID already exists (idempotent retry)
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
        return Err(anyhow!(
            "Event with ID {} already exists with different content",
            event.event_id
        ));
    }

    // Read the item's current revision (authoritative source for compare-and-set)
    let current_item_revision: i32 = tx
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [event.item_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Item {} not found", event.item_id))?;

    // Compare-and-set: reject if expected revision doesn't match current
    if expected_item_revision != current_item_revision {
        return Err(anyhow!(
            "Stale write: expected item {} revision {} but current is {}",
            event.item_id,
            expected_item_revision,
            current_item_revision
        ));
    }

    // Insert the event with payload fields
    let (correction_kind, correction_old_value, correction_new_value, suggestion_control_kind) =
        match &event.payload {
            EventPayload::Correction(c) => (
                Some(c.kind.clone()),
                c.old_value.clone(),
                Some(c.new_value.clone()),
                None,
            ),
            EventPayload::SuggestionControl(s) => (None, None, None, Some(s.kind.clone())),
            EventPayload::Completion => (None, None, None, None),
            EventPayload::Cancellation => (None, None, None, None),
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

    // If this is a Correction event, also write to the corrections table
    if let EventPayload::Correction(corr) = &event.payload {
        let correction_id = format!("{}-correction", event.event_id);
        tx.execute(
            "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                &correction_id,
                &event.item_id,
                event.revision,
                &corr.kind,
                &corr.old_value,
                &corr.new_value,
                &event.happened_at,
            ],
        )?;
    }

    // Apply lifecycle state changes
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
        _ => {}
    }

    // Atomically increment the item's revision
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
    tx.execute("DELETE FROM events WHERE item_id = ?", [item_id])
        .map_err(|e| anyhow!(e))
}
