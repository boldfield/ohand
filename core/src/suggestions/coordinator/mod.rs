// Prompt activation and selection (S03).
//
// When the user opens a daily prompt (an authenticated session opening a notification whose
// identifier names a prompt), this module resolves what is eligible *now* and offers at most one
// item with three simple responses.
//
// - Nothing is preselected. The prompt notification carries no item (S02 payloads are generic),
//   and a selection recorded by an earlier activation is only rotation history. Eligibility is
//   evaluated again over current authoritative state, so an item deleted, completed, cancelled,
//   snoozed, muted or given a reminder since then can never be offered as current.
// - No eligible item is a calm, countless `NothingToOffer`: no backlog, no suggestion to clean
//   up an inbox.
// - Responses reuse the existing revisioned operations and never invent state: not-now is
//   `eligibility::record_not_now` (control event plus the stated cooldown), stop-suggesting is a
//   control event plus the durable pull-only mark, and done is the completion event applied by
//   `end_item_with_event`. Every response compares the item revision the user saw; a stale
//   response reports the current revision instead of overwriting newer state.
// - Muting optional help (not-now, stop-suggesting) writes no reminder, prompt-schedule or
//   notification state, so explicit reminders stay exactly as they were. Done completes the
//   item, which is the existing meaning of completion: its own reminder, if any, is cancelled in
//   the same transaction and the next settlement removes the pending OS request.
// - Responses are idempotent per `command_id`: the event identifier derives from it, so a retry
//   after a lost acknowledgment reports `Applied { replayed: true }` and writes nothing.
// - Both entry points require `ReadAuthentication::Authenticated` and read nothing otherwise.

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::OptionalExtension;
use thiserror::Error;

use crate::domain::items::load_item_state;
use crate::export::ReadAuthentication;
use crate::reminders::state::{end_item_with_event, ReminderStateError};
use crate::store::events::{
    get_event, get_item_snapshot, save_event_in_tx, Event, EventError, EventPayload, EventType,
    ItemScope, SuggestionControlKind, SuggestionControlPayload,
};
use crate::store::schema::{Clock, Database};
use crate::suggestions::eligibility::{record_not_now, select_eligible_item, set_pull_only};
use crate::suggestions::schedule::is_prompt_notification_id;

/// A response the user can give to an offered item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptResponse {
    /// Not now: the item is left out of suggestions for the stated cooldown.
    NotNow,
    /// Stop suggesting this item: it stays searchable and is never offered again.
    StopSuggesting,
    /// The item is done.
    Done,
}

/// Every response offered with an item, in display order.
pub const PROMPT_RESPONSES: [PromptResponse; 3] = [
    PromptResponse::NotNow,
    PromptResponse::StopSuggesting,
    PromptResponse::Done,
];

/// The one item offered by an activation. Carries private text, so it exists only inside an
/// authenticated session and its `Debug` output omits the text.
#[derive(Clone, PartialEq, Eq)]
pub struct PromptOffer {
    pub item_id: String,
    /// The revision the user is shown; responses must state it.
    pub revision: i32,
    pub text: Option<String>,
    pub scope: ItemScope,
    /// Why the item was eligible, as stored by the selection.
    pub selection_reason: String,
    pub responses: [PromptResponse; 3],
}

impl fmt::Debug for PromptOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptOffer")
            .field("item_id", &self.item_id)
            .field("revision", &self.revision)
            .field("scope", &self.scope)
            .field("selection_reason", &self.selection_reason)
            .finish_non_exhaustive()
    }
}

/// The result of opening a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptActivation {
    Offer(PromptOffer),
    /// Nothing is eligible right now. Deliberately carries no count or backlog.
    NothingToOffer,
}

/// A user's response to an offered item. `command_id` is the immutable identity of this action,
/// reused unchanged by every retry of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptResponseCommand {
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    pub response: PromptResponse,
}

/// The result of a response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseOutcome {
    /// The response is stored. `replayed` is true when an earlier attempt of the same command
    /// had already committed it.
    Applied { replayed: bool },
    /// The item is deleted, completed, cancelled or gone, so there is nothing to respond to.
    NoLongerCurrent,
    /// The item changed since the user saw it. Nothing was written.
    Stale { current_revision: i32 },
}

#[derive(Debug, Error)]
pub enum PromptCoordinatorError {
    #[error("prompt activation and responses require an authenticated read session")]
    NotAuthenticated,
    #[error("notification {0} is not a daily prompt")]
    NotAPrompt(String),
    #[error("command {command_id} was already used for a different response")]
    CommandConflict { command_id: String },
    #[error(transparent)]
    Event(#[from] EventError),
    #[error(transparent)]
    Reminder(#[from] ReminderStateError),
    #[error("storage error: {0}")]
    Storage(String),
}

fn storage_error(error: impl fmt::Display) -> PromptCoordinatorError {
    PromptCoordinatorError::Storage(error.to_string())
}

fn require_authenticated(authentication: ReadAuthentication) -> Result<(), PromptCoordinatorError> {
    match authentication {
        ReadAuthentication::Authenticated => Ok(()),
        ReadAuthentication::CaptureOnly => Err(PromptCoordinatorError::NotAuthenticated),
    }
}

pub struct PromptCoordinator<'a> {
    clock: &'a dyn Clock,
}

impl<'a> PromptCoordinator<'a> {
    /// `clock` stamps the selection, the cooldown and the response events.
    pub fn new(clock: &'a dyn Clock) -> PromptCoordinator<'a> {
        PromptCoordinator { clock }
    }

    /// Resolve current eligibility for the opened prompt `notification_id` and offer at most
    /// one item. `scope_filter` limits the offer to one read scope; `None` allows every scope
    /// the authenticated session may read. Offering rotates: the offered item is stamped as
    /// selected, so the next activation prefers another eligible item.
    pub fn activate(
        &self,
        database: &mut Database,
        authentication: ReadAuthentication,
        notification_id: &str,
        scope_filter: Option<ItemScope>,
    ) -> Result<PromptActivation, PromptCoordinatorError> {
        require_authenticated(authentication)?;
        if !is_prompt_notification_id(notification_id) {
            return Err(PromptCoordinatorError::NotAPrompt(
                notification_id.to_string(),
            ));
        }
        let transaction = database.immediate_transaction().map_err(storage_error)?;
        let selected = select_eligible_item(&transaction, self.clock.now(), scope_filter)
            .map_err(storage_error)?;
        let Some(item_id) = selected else {
            return Ok(PromptActivation::NothingToOffer);
        };
        let state = load_item_state(&transaction, &item_id)
            .map_err(storage_error)?
            .ok_or_else(|| storage_error(format!("selected item {item_id} has no state")))?;
        let selection_reason: String = transaction
            .query_row(
                "SELECT selection_reason FROM suggestion_eligibility WHERE item_id = ?",
                [&item_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage_error)?
            .flatten()
            .ok_or_else(|| storage_error(format!("selected item {item_id} has no reason")))?;
        transaction.commit().map_err(storage_error)?;
        Ok(PromptActivation::Offer(PromptOffer {
            item_id,
            revision: state.revision,
            text: state.current_text.text().map(str::to_string),
            scope: state.scope,
            selection_reason,
            responses: PROMPT_RESPONSES,
        }))
    }

    /// Apply the user's response to the offered item, in one transaction.
    pub fn respond(
        &self,
        database: &mut Database,
        authentication: ReadAuthentication,
        command: &PromptResponseCommand,
    ) -> Result<ResponseOutcome, PromptCoordinatorError> {
        require_authenticated(authentication)?;
        let transaction = database.immediate_transaction().map_err(storage_error)?;
        let event_id = format!("prompt-response:{}", command.command_id);

        if let Some(existing) = get_event(&transaction, &event_id).map_err(storage_error)? {
            return if existing.item_id == command.item_id
                && event_matches_response(&existing, command.response)
            {
                Ok(ResponseOutcome::Applied { replayed: true })
            } else {
                Err(PromptCoordinatorError::CommandConflict {
                    command_id: command.command_id.clone(),
                })
            };
        }

        let Some(snapshot) = get_item_snapshot(&transaction, &command.item_id)? else {
            return Ok(ResponseOutcome::NoLongerCurrent);
        };
        if snapshot.lifecycle_state != "active" {
            return Ok(ResponseOutcome::NoLongerCurrent);
        }
        if snapshot.revision != command.expected_revision {
            return Ok(ResponseOutcome::Stale {
                current_revision: snapshot.revision,
            });
        }

        let now = self.clock.now();
        let revision = command.expected_revision;
        match command.response {
            PromptResponse::NotNow => {
                record_not_now(&transaction, &command.item_id, &event_id, revision, now)
                    .map_err(storage_error)?;
            }
            PromptResponse::StopSuggesting => {
                let event = response_event(&event_id, command, now)?;
                save_event_in_tx(&transaction, &event, revision)?;
                set_pull_only(&transaction, &command.item_id, now).map_err(storage_error)?;
            }
            PromptResponse::Done => {
                let event = response_event(&event_id, command, now)?;
                end_item_with_event(&transaction, self.clock, &event, revision)?;
            }
        }
        transaction.commit().map_err(storage_error)?;
        Ok(ResponseOutcome::Applied { replayed: false })
    }
}

fn event_matches_response(event: &Event, response: PromptResponse) -> bool {
    let stored = match &event.payload {
        EventPayload::SuggestionControl(control) => match control.kind {
            SuggestionControlKind::NotNow => PromptResponse::NotNow,
            SuggestionControlKind::StopSuggesting => PromptResponse::StopSuggesting,
        },
        EventPayload::Completion => PromptResponse::Done,
        _ => return false,
    };
    stored == response
}

fn response_event(
    event_id: &str,
    command: &PromptResponseCommand,
    now: DateTime<Utc>,
) -> Result<Event, PromptCoordinatorError> {
    let (event_type, payload) = match command.response {
        PromptResponse::NotNow => (
            EventType::SuggestionControl,
            EventPayload::SuggestionControl(SuggestionControlPayload {
                kind: SuggestionControlKind::NotNow,
            }),
        ),
        PromptResponse::StopSuggesting => (
            EventType::SuggestionControl,
            EventPayload::SuggestionControl(SuggestionControlPayload {
                kind: SuggestionControlKind::StopSuggesting,
            }),
        ),
        PromptResponse::Done => (EventType::Completion, EventPayload::Completion),
    };
    Event::new(
        event_id.to_string(),
        command.item_id.clone(),
        command.expected_revision,
        event_type,
        payload,
        now.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
    .map_err(storage_error)
}
