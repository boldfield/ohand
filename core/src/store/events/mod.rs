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
pub struct Event {
    pub event_id: String,
    pub item_id: String,
    pub revision: i32,
    pub event_type: EventType,
    pub happened_at: String,
}

impl Event {
    pub fn new(
        event_id: String,
        item_id: String,
        revision: i32,
        event_type: EventType,
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
            happened_at,
        })
    }
}

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

    Ok(Event {
        event_id: row.get(0)?,
        item_id: row.get(1)?,
        revision: row.get(2)?,
        event_type,
        happened_at: row.get(4)?,
    })
}

/// Save an event with compare-and-set revision semantics.
/// Returns the saved event if successful.
///
/// If an event with the same ID already exists:
/// - Returns the existing event if it is identical (idempotent retry)
/// - Returns an error if it conflicts (different item_id, revision, or event_type)
///
/// If a different event with a newer or equal revision for the same item already exists,
/// the operation fails to prevent stale updates from overwriting newer state.
pub fn save_event(db: &mut Database, event: &Event) -> Result<Event> {
    let tx = db.immediate_transaction()?;
    let result = save_event_in_tx(&tx, event)?;
    tx.commit()?;
    Ok(result)
}

/// Save an event within a caller-owned transaction.
/// This is a lower-level helper; prefer `save_event` for durable operations.
///
/// Compare-and-set semantics: conflicting stale updates are rejected.
pub fn save_event_in_tx(tx: &Transaction<'_>, event: &Event) -> Result<Event> {
    // Check if event with same ID already exists
    let existing: Option<Event> = tx
        .query_row(
            "SELECT event_id, item_id, revision, event_type, happened_at FROM events WHERE event_id = ?",
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

    // Check for conflicting event at the same revision: if there's already an event
    // for this item at this revision, reject it as a conflict (stale update).
    let existing_at_revision: Option<String> = tx
        .query_row(
            "SELECT event_id FROM events WHERE item_id = ? AND revision = ?",
            rusqlite::params![event.item_id.as_str(), event.revision],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(existing_id) = existing_at_revision {
        if existing_id != event.event_id {
            return Err(anyhow!(
                "Stale event: item {} already has event {} at revision {}",
                event.item_id,
                existing_id,
                event.revision
            ));
        }
    }

    // Also reject if trying to insert at a revision lower than an existing one
    // (this prevents retroactively inserting events into the past).
    let max_revision: i32 = tx
        .query_row(
            "SELECT COALESCE(MAX(revision), -1) FROM events WHERE item_id = ?",
            [event.item_id.as_str()],
            |row| row.get(0),
        )?;

    if event.revision < max_revision {
        return Err(anyhow!(
            "Stale event: item {} already has event at revision {} (trying to insert at {})",
            event.item_id,
            max_revision,
            event.revision
        ));
    }

    tx.execute(
        "INSERT INTO events (event_id, item_id, revision, event_type, happened_at)
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            &event.event_id,
            &event.item_id,
            event.revision,
            event.event_type.as_str(),
            &event.happened_at,
        ],
    )?;

    Ok(event.clone())
}

/// Retrieve an event by ID.
pub fn get_event(tx: &Transaction<'_>, event_id: &str) -> Result<Option<Event>> {
    tx.query_row(
        "SELECT event_id, item_id, revision, event_type, happened_at FROM events WHERE event_id = ?",
        [event_id],
        event_from_row,
    )
    .optional()
    .map_err(|e| anyhow!(e))
}

/// Retrieve all events for an item, ordered by revision ascending.
pub fn get_events_for_item(tx: &Transaction<'_>, item_id: &str) -> Result<Vec<Event>> {
    let mut stmt = tx.prepare(
        "SELECT event_id, item_id, revision, event_type, happened_at FROM events
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
