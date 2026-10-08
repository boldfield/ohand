// Eligibility scoring and rotation
// S01: Select from eligible undated actions without fabricated urgency, with stored reasons
// and deterministic clock-controlled rotation.

use crate::domain::items::LifecycleState;
use crate::store::events::{self, SuggestionControlKind};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction};

/// Reason why an item is eligible or ineligible for suggestions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EligibilityReason {
    /// Item is active and can carry obligations
    ActiveAction,
    /// Item is completed (not eligible)
    Completed,
    /// Item is cancelled (not eligible)
    Cancelled,
    /// Item is deleted (not eligible)
    Deleted,
    /// Item was not typed as an action (not eligible)
    NotAnAction,
    /// Item has no clear interpretation (not eligible)
    Uninterpreted,
    /// User marked as "not now"; cooldown active until snoozed_until
    SnoozedUntil(String),
    /// User marked "stop suggesting"; pull-only (not eligible)
    StopSuggesting,
}

impl EligibilityReason {
    pub fn as_str(&self) -> &str {
        match self {
            EligibilityReason::ActiveAction => "active_action",
            EligibilityReason::Completed => "completed",
            EligibilityReason::Cancelled => "cancelled",
            EligibilityReason::Deleted => "deleted",
            EligibilityReason::NotAnAction => "not_an_action",
            EligibilityReason::Uninterpreted => "uninterpreted",
            EligibilityReason::SnoozedUntil(_) => "snoozed_until",
            EligibilityReason::StopSuggesting => "stop_suggesting",
        }
    }
}

/// Eligibility status for an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eligibility {
    /// Whether the item is eligible for proactive suggestions
    pub eligible: bool,
    /// The reason for the eligibility status
    pub reason: EligibilityReason,
}

/// Check whether an item is eligible for proactive suggestions.
/// Returns eligibility status and the reason.
///
/// Excludes:
/// - Completed, cancelled, deleted items
/// - Items not typed as actions
/// - Items with uninterpreted intent
/// - Items with "stop suggesting" control
/// - Items with active "not now" cooldown
pub fn check_eligibility(tx: &Transaction<'_>, item_id: &str) -> Result<Eligibility> {
    // Load item state
    let item_state = crate::domain::items::load_item_state(tx, item_id)?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;

    // Check lifecycle state
    match item_state.lifecycle_state {
        LifecycleState::Completed => {
            return Ok(Eligibility {
                eligible: false,
                reason: EligibilityReason::Completed,
            });
        }
        LifecycleState::Cancelled => {
            return Ok(Eligibility {
                eligible: false,
                reason: EligibilityReason::Cancelled,
            });
        }
        LifecycleState::Deleted => {
            return Ok(Eligibility {
                eligible: false,
                reason: EligibilityReason::Deleted,
            });
        }
        LifecycleState::Active => {}
    }

    // Check if item was interpreted and typed as an action
    match item_state.item_type {
        None => {
            return Ok(Eligibility {
                eligible: false,
                reason: EligibilityReason::Uninterpreted,
            });
        }
        Some(item_type) => {
            if !item_type.can_carry_obligation() {
                return Ok(Eligibility {
                    eligible: false,
                    reason: EligibilityReason::NotAnAction,
                });
            }
        }
    }

    // Check suggestion control events for "stop suggesting"
    let events = events::get_events_for_item(tx, item_id)?;
    let mut last_suggestion_control: Option<(SuggestionControlKind, String)> = None;

    for event in &events {
        if let crate::store::events::EventType::SuggestionControl = event.event_type {
            if let crate::store::events::EventPayload::SuggestionControl(payload) = &event.payload {
                last_suggestion_control = Some((payload.kind.clone(), event.happened_at.clone()));
            }
        }
    }

    // Check if latest suggestion control is "stop suggesting"
    if let Some((kind, _)) = last_suggestion_control {
        match kind {
            SuggestionControlKind::StopSuggesting => {
                return Ok(Eligibility {
                    eligible: false,
                    reason: EligibilityReason::StopSuggesting,
                });
            }
            SuggestionControlKind::NotNow => {
                // Check if the snooze is still active
                if let Some(snoozed_until) = get_snooze_expiry(tx, item_id)? {
                    return Ok(Eligibility {
                        eligible: false,
                        reason: EligibilityReason::SnoozedUntil(snoozed_until.to_rfc3339()),
                    });
                }
            }
        }
    }

    Ok(Eligibility {
        eligible: true,
        reason: EligibilityReason::ActiveAction,
    })
}

/// Get the snooze expiry time for an item, or None if not snoozed or expired.
/// Returns the time when the item's snooze expires, or None if no active snooze.
fn get_snooze_expiry(tx: &Transaction<'_>, item_id: &str) -> Result<Option<DateTime<Utc>>> {
    // Query the suggestion_eligibility table for snoozed_until
    let snoozed_until: Option<Option<String>> = tx
        .query_row(
            "SELECT snoozed_until FROM suggestion_eligibility WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(Some(snoozed_until_str)) = snoozed_until {
        match snoozed_until_str.parse::<DateTime<Utc>>() {
            Ok(expires_at) => {
                // Check if snooze is still active
                if expires_at > Utc::now() {
                    return Ok(Some(expires_at));
                }
            }
            Err(_) => {
                // Invalid timestamp, treat as no snooze
            }
        }
    }
    Ok(None)
}

/// Record that an item was selected for suggestion with a reason.
/// Updates the suggestion_eligibility table with selection timestamp and reason.
pub fn record_selection(
    tx: &Transaction<'_>,
    item_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let now_str = now.to_rfc3339();
    tx.execute(
        "INSERT INTO suggestion_eligibility (item_id, eligible, snoozed, pull_only, last_selected_at, selection_reason, created_at, updated_at)
         VALUES (?, 1, 0, 0, ?, ?, ?, ?)
         ON CONFLICT(item_id) DO UPDATE SET
           last_selected_at = ?,
           selection_reason = ?,
           updated_at = ?",
        rusqlite::params![
            item_id,
            &now_str,
            reason,
            &now_str,
            &now_str,
            &now_str,
            reason,
            &now_str,
        ],
    )?;
    Ok(())
}

/// Set a snooze (not-now) for an item with the specified cooldown duration.
/// The snooze expires after the given duration.
pub fn set_snooze(
    tx: &Transaction<'_>,
    item_id: &str,
    cooldown: Duration,
    now: DateTime<Utc>,
) -> Result<()> {
    let expires_at = now + cooldown;
    let expires_at_str = expires_at.to_rfc3339();
    let now_str = now.to_rfc3339();

    tx.execute(
        "INSERT INTO suggestion_eligibility (item_id, eligible, snoozed, pull_only, snoozed_until, created_at, updated_at)
         VALUES (?, 0, 1, 0, ?, ?, ?)
         ON CONFLICT(item_id) DO UPDATE SET
           snoozed = 1,
           snoozed_until = ?,
           eligible = 0,
           updated_at = ?",
        rusqlite::params![
            item_id,
            &expires_at_str,
            &now_str,
            &now_str,
            &expires_at_str,
            &now_str,
        ],
    )?;
    Ok(())
}

/// Mark an item as pull-only (stop suggesting).
/// This state is durable and searchable, but never eligible for proactive suggestions.
pub fn set_pull_only(tx: &Transaction<'_>, item_id: &str, now: DateTime<Utc>) -> Result<()> {
    let now_str = now.to_rfc3339();
    tx.execute(
        "INSERT INTO suggestion_eligibility (item_id, eligible, snoozed, pull_only, created_at, updated_at)
         VALUES (?, 0, 0, 1, ?, ?)
         ON CONFLICT(item_id) DO UPDATE SET
           pull_only = 1,
           eligible = 0,
           snoozed = 0,
           snoozed_until = NULL,
           updated_at = ?",
        rusqlite::params![item_id, &now_str, &now_str, &now_str],
    )?;
    Ok(())
}

/// List all active eligible items ordered by rotation.
/// Items are ordered by last_selected_at (oldest first) to rotate through them.
pub fn list_eligible_items(tx: &Transaction<'_>) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT se.item_id FROM suggestion_eligibility se
         JOIN items i ON i.item_id = se.item_id
         WHERE se.eligible = 1
           AND i.lifecycle_state = 'active'
           AND i.item_type IS NOT NULL
         ORDER BY se.last_selected_at ASC NULLS FIRST, se.item_id ASC",
    )?;

    let items = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(items)
}
