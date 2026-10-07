// Idempotent durable raw capture storage.
// Implementation owned by D02.

use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;

/// A capture record holding source text/audio references and context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub capture_id: String,
    pub text: Option<String>,
    pub audio_reference: Option<String>,
    pub capture_instant: String,
    pub timezone_id: String,
    pub utc_offset_minutes: i32,
    pub locale: String,
    pub calendar: String,
    pub item_scope: String,
    pub route_id: String,
    pub entry_locked: bool,
    pub created_at: String,
    pub session_topic: Option<String>,
}

impl Capture {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capture_id: String,
        text: Option<String>,
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
        session_topic: Option<String>,
    ) -> Result<Self> {
        if text.is_none() && audio_reference.is_none() {
            return Err(anyhow!(
                "Capture must have either text or audio_reference (or both)"
            ));
        }
        Ok(Capture {
            capture_id,
            text,
            audio_reference,
            capture_instant,
            timezone_id,
            utc_offset_minutes,
            locale,
            calendar,
            item_scope,
            route_id,
            entry_locked,
            created_at,
            session_topic,
        })
    }
}

impl fmt::Display for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Capture({})", self.capture_id)
    }
}

/// Idempotent save of a capture. Returns the saved capture (which may be the existing
/// record if an identical one was already saved).
///
/// If a different capture with the same ID already exists, returns an error.
/// The save is atomic: durability is guaranteed only after successful return.
pub fn save_capture(tx: &Transaction<'_>, capture: &Capture) -> Result<Capture> {
    // Check if a capture with this ID already exists.
    let existing: Option<Capture> = tx
        .query_row(
            "SELECT capture_id, text, audio_reference, capture_instant, timezone_id,
                    utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked,
                    created_at, session_topic
             FROM captures WHERE capture_id = ?",
            [capture.capture_id.as_str()],
            |row| {
                Ok(Capture {
                    capture_id: row.get(0)?,
                    text: row.get(1)?,
                    audio_reference: row.get(2)?,
                    capture_instant: row.get(3)?,
                    timezone_id: row.get(4)?,
                    utc_offset_minutes: row.get(5)?,
                    locale: row.get(6)?,
                    calendar: row.get(7)?,
                    item_scope: row.get(8)?,
                    route_id: row.get(9)?,
                    entry_locked: row.get::<_, i32>(10)? != 0,
                    created_at: row.get(11)?,
                    session_topic: row.get(12)?,
                })
            },
        )
        .optional()?;

    if let Some(existing_capture) = existing {
        // If an identical capture exists, return it (idempotent).
        if existing_capture == *capture {
            return Ok(existing_capture);
        }
        // If different capture with same ID exists, fail rather than replacing.
        return Err(anyhow!(
            "Capture with ID {} already exists with different content",
            capture.capture_id
        ));
    }

    // Insert the new capture.
    tx.execute(
        "INSERT INTO captures (
            capture_id, text, audio_reference, capture_instant, timezone_id,
            utc_offset_minutes, locale, calendar, item_scope, route_id,
            entry_locked, created_at, session_topic
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &capture.capture_id,
            &capture.text,
            &capture.audio_reference,
            &capture.capture_instant,
            &capture.timezone_id,
            capture.utc_offset_minutes,
            &capture.locale,
            &capture.calendar,
            &capture.item_scope,
            &capture.route_id,
            if capture.entry_locked { 1 } else { 0 },
            &capture.created_at,
            &capture.session_topic,
        ],
    )?;

    Ok(capture.clone())
}

/// Retrieve a capture by ID.
pub fn get_capture(tx: &Transaction<'_>, capture_id: &str) -> Result<Option<Capture>> {
    tx.query_row(
        "SELECT capture_id, text, audio_reference, capture_instant, timezone_id,
                utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked,
                created_at, session_topic
         FROM captures WHERE capture_id = ?",
        [capture_id],
        |row| {
            Ok(Capture {
                capture_id: row.get(0)?,
                text: row.get(1)?,
                audio_reference: row.get(2)?,
                capture_instant: row.get(3)?,
                timezone_id: row.get(4)?,
                utc_offset_minutes: row.get(5)?,
                locale: row.get(6)?,
                calendar: row.get(7)?,
                item_scope: row.get(8)?,
                route_id: row.get(9)?,
                entry_locked: row.get::<_, i32>(10)? != 0,
                created_at: row.get(11)?,
                session_topic: row.get(12)?,
            })
        },
    )
    .optional()
    .map_err(|e| anyhow!(e))
}
