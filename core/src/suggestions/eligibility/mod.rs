// Eligibility scoring and rotation
// S01: Select from eligible undated actions without fabricated urgency, with stored reasons
// and deterministic clock-controlled rotation.

use crate::domain::items::LifecycleState;
use crate::store::events::{self, SuggestionControlKind};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction};

/// Named not-now cooldown policy: 1 hour by default
pub const NOT_NOW_COOLDOWN: Duration = Duration::hours(1);

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
    /// Item has a reminder/is dated (not eligible for undated suggestions)
    HasReminder,
    /// Item has no clear interpretation (not eligible)
    Uninterpreted,
    /// Item scope doesn't match query filter (not eligible)
    ScopeExcluded,
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
            EligibilityReason::HasReminder => "has_reminder",
            EligibilityReason::Uninterpreted => "uninterpreted",
            EligibilityReason::ScopeExcluded => "scope_excluded",
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

/// Candidate item for rotation selection.
#[derive(Clone, Debug)]
struct EligibleItemCandidate {
    /// Item identifier
    item_id: String,
    /// Last time this item was selected (None if never selected)
    last_selected_at: Option<DateTime<Utc>>,
    /// Selection sequence for tie-breaking when timestamps are equal
    selection_sequence: Option<i32>,
    /// Reason why this item is eligible
    reason: String,
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
/// - Items marked as pull-only (stop suggesting via durable state)
/// - Items with reminders (dated/reminder-bearing actions)
/// - Items whose scope doesn't match the optional filter
///
/// `evaluation_instant` is used for deterministic cooldown evaluation (for testing).
/// `scope_filter` is an optional item scope filter (e.g., for privacy boundaries).
pub fn check_eligibility_with_scope(
    tx: &Transaction<'_>,
    item_id: &str,
    evaluation_instant: DateTime<Utc>,
    scope_filter: Option<&str>,
) -> Result<Eligibility> {
    // Load item state
    let item_state = crate::domain::items::load_item_state(tx, item_id)?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;

    // Check scope filter if provided (privacy eligibility)
    if let Some(filter) = scope_filter {
        let item_scope = item_state.scope.as_str();
        if item_scope != filter {
            return Ok(Eligibility {
                eligible: false,
                reason: EligibilityReason::ScopeExcluded,
            });
        }
    }

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

    // Check for reminders (dated actions are not eligible for suggestions)
    let has_reminder: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM reminders WHERE item_id = ?)",
        [item_id],
        |row| row.get(0),
    )?;

    if has_reminder {
        return Ok(Eligibility {
            eligible: false,
            reason: EligibilityReason::HasReminder,
        });
    }

    // Check durable pull_only state (stop suggesting recorded in suggestion_eligibility table)
    let pull_only_state: Option<i32> = tx
        .query_row(
            "SELECT pull_only FROM suggestion_eligibility WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;

    if pull_only_state == Some(1) {
        return Ok(Eligibility {
            eligible: false,
            reason: EligibilityReason::StopSuggesting,
        });
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
                if let Some(snoozed_until) = get_snooze_expiry(tx, item_id, evaluation_instant)? {
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

/// Check whether an item is eligible for proactive suggestions (public convenience wrapper).
/// Equivalent to check_eligibility_with_scope with no scope filter.
pub fn check_eligibility(
    tx: &Transaction<'_>,
    item_id: &str,
    evaluation_instant: DateTime<Utc>,
) -> Result<Eligibility> {
    check_eligibility_with_scope(tx, item_id, evaluation_instant, None)
}

/// Get the snooze expiry time for an item, or None if not snoozed or expired.
/// Evaluates snooze status at the provided evaluation instant (for deterministic testing).
/// Returns the time when the item's snooze expires, or an error if the stored timestamp is malformed.
fn get_snooze_expiry(
    tx: &Transaction<'_>,
    item_id: &str,
    evaluation_instant: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    // Query the suggestion_eligibility table for snoozed_until
    let snoozed_until: Option<Option<String>> = tx
        .query_row(
            "SELECT snoozed_until FROM suggestion_eligibility WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(Some(snoozed_until_str)) = snoozed_until {
        let expires_at = snoozed_until_str
            .parse::<DateTime<Utc>>()
            .map_err(|_| anyhow!("Invalid snoozed_until timestamp: {}", snoozed_until_str))?;
        // Check if snooze is still active at evaluation instant
        if expires_at > evaluation_instant {
            return Ok(Some(expires_at));
        }
    }
    Ok(None)
}

/// Record that an item was selected for suggestion with a reason.
/// Updates the suggestion_eligibility table with selection timestamp, reason, and sequence.
/// The selection_sequence increments to ensure rotation advances even when clock doesn't.
pub fn record_selection(
    tx: &Transaction<'_>,
    item_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let now_str = now.to_rfc3339();

    // Get the next sequence number (max sequence + 1, or 1 if no records yet)
    let next_sequence: i32 = tx
        .query_row(
            "SELECT COALESCE(MAX(selection_sequence), 0) + 1 FROM suggestion_eligibility",
            [],
            |row| row.get(0),
        )
        .unwrap_or(1);

    tx.execute(
        "INSERT INTO suggestion_eligibility (item_id, eligible, snoozed, pull_only, last_selected_at, selection_reason, selection_sequence, created_at, updated_at)
         VALUES (?, 1, 0, 0, ?, ?, ?, ?, ?)
         ON CONFLICT(item_id) DO UPDATE SET
           last_selected_at = ?,
           selection_reason = ?,
           selection_sequence = ?,
           eligible = 1,
           snoozed = 0,
           updated_at = ?",
        rusqlite::params![
            item_id,
            &now_str,
            reason,
            next_sequence,
            &now_str,
            &now_str,
            &now_str,
            reason,
            next_sequence,
            &now_str,
        ],
    )?;
    Ok(())
}

/// Set a snooze (not-now) for an item with the specified cooldown duration (internal use).
/// The snooze expires after the given duration from the provided instant.
fn set_snooze(
    tx: &Transaction<'_>,
    item_id: &str,
    cooldown: Duration,
    evaluation_instant: DateTime<Utc>,
) -> Result<()> {
    let expires_at = evaluation_instant + cooldown;
    let expires_at_str = expires_at.to_rfc3339();
    let instant_str = evaluation_instant.to_rfc3339();

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
            &instant_str,
            &instant_str,
            &expires_at_str,
            &instant_str,
        ],
    )?;
    Ok(())
}

/// Record a "not now" response and apply its cooldown atomically.
/// Saves the NotNow SuggestionControl event and applies the cooldown in one transaction.
/// The cooldown is anchored at the event's happened_at time.
///
/// This is the authoritative API for "not now" responses. Callers should not
/// separately call set_snooze or save the event; this function handles both.
pub fn record_not_now(
    tx: &Transaction<'_>,
    item_id: &str,
    event_id: &str,
    current_revision: i32,
    event_time: DateTime<Utc>,
) -> Result<()> {
    // Validate the item exists
    let _item_state = crate::domain::items::load_item_state(tx, item_id)?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;

    // Create and save the NotNow SuggestionControl event
    let event = events::Event::new(
        event_id.to_string(),
        item_id.to_string(),
        current_revision,
        events::EventType::SuggestionControl,
        events::EventPayload::SuggestionControl(events::SuggestionControlPayload {
            kind: SuggestionControlKind::NotNow,
        }),
        event_time.to_rfc3339(),
    )?;

    events::save_event_in_tx(tx, &event, current_revision)?;

    // Apply the cooldown anchored at the event time
    set_snooze(tx, item_id, NOT_NOW_COOLDOWN, event_time)?;

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

/// Select an eligible item for suggestion using deterministic rotation.
/// Returns the item_id of the selected item, if any are eligible.
///
/// This is the main entry point for suggestion rotation. It:
/// - Enforces eligibility policy over current authoritative state
/// - Performs deterministic rotation (oldest last_selected_at first)
/// - Records the selection reason durably (the actual reason the item was eligible)
/// - Takes evaluation time as a parameter for deterministic testing
/// - Accepts an optional scope filter for privacy boundaries
///
/// Returns None if no eligible items are available.
pub fn select_eligible_item(
    tx: &Transaction<'_>,
    evaluation_instant: DateTime<Utc>,
    scope_filter: Option<&str>,
) -> Result<Option<String>> {
    // Find all active items that are not deleted
    let mut stmt = tx.prepare(
        "SELECT item_id FROM items
         WHERE lifecycle_state = 'active'
         ORDER BY item_id",
    )?;

    let items: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    // Check each item for eligibility, tracking the best candidate
    let mut eligible_items: Vec<EligibleItemCandidate> = Vec::new();

    for item_id in items {
        match check_eligibility_with_scope(tx, &item_id, evaluation_instant, scope_filter) {
            Ok(eligibility) if eligibility.eligible => {
                // Get the last selection time and sequence for rotation ordering
                let (last_selected_at_str, sequence): (Option<String>, Option<i32>) = tx
                    .query_row(
                        "SELECT last_selected_at, selection_sequence FROM suggestion_eligibility WHERE item_id = ?",
                        [&item_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .unwrap_or((None, None));

                let last_selected_dt = match last_selected_at_str {
                    Some(s) => Some(
                        s.parse::<DateTime<Utc>>()
                            .map_err(|e| anyhow!("Invalid last_selected_at: {}", e))?,
                    ),
                    None => None,
                };

                // Store the actual reason the item was eligible
                let reason_str = eligibility.reason.as_str().to_string();
                eligible_items.push(EligibleItemCandidate {
                    item_id,
                    last_selected_at: last_selected_dt,
                    selection_sequence: sequence,
                    reason: reason_str,
                });
            }
            Ok(_) => {
                // Item is not eligible, skip it
            }
            Err(e) => {
                // Propagate store/parse errors; don't silently skip
                return Err(e);
            }
        }
    }

    if eligible_items.is_empty() {
        return Ok(None);
    }

    // Sort by last_selected_at (None values first = oldest), then by selection_sequence, then by item_id
    eligible_items.sort_by(|a, b| match (a.last_selected_at, b.last_selected_at) {
        (None, None) => match (a.selection_sequence, b.selection_sequence) {
            (None, None) => a.item_id.cmp(&b.item_id),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(sa), Some(sb)) => match sa.cmp(&sb) {
                std::cmp::Ordering::Equal => a.item_id.cmp(&b.item_id),
                other => other,
            },
        },
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(ta), Some(tb)) => match ta.cmp(&tb) {
            std::cmp::Ordering::Equal => match (a.selection_sequence, b.selection_sequence) {
                (None, None) => a.item_id.cmp(&b.item_id),
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(sa), Some(sb)) => match sa.cmp(&sb) {
                    std::cmp::Ordering::Equal => a.item_id.cmp(&b.item_id),
                    other => other,
                },
            },
            other => other,
        },
    });

    // Select the first item (oldest last_selected_at, then oldest selection_sequence)
    if let Some(candidate) = eligible_items.first() {
        record_selection(
            tx,
            &candidate.item_id,
            &candidate.reason,
            evaluation_instant,
        )?;
        return Ok(Some(candidate.item_id.clone()));
    }

    Ok(None)
}
