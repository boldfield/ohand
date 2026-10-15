//! Prompt activation and the three responses over the core handle (A06).
//!
//! `ohand_core_start_suggestion_response` carries one tagged JSON command, runs it on the handle's
//! worker thread through `PromptCoordinator` (S03), and answers `{"operation_id", "outcome", ...}`.
//! Every command carries `read_authentication` (`authenticated` | `capture_only`), the native
//! session read scope. Any other value is `invalid_request`; `capture_only` is refused as
//! `unauthorized` before the store is touched, so nothing stored is disclosed or changed.
//!
//! * `activate_prompt` (`notification_id`, optional `scope`: `personal` | `work`): resolves what is
//!   eligible now for the opened prompt and offers at most one item.
//!   * `offer` (`item_id`, `revision` the user is shown, `text` (null when the item has no text,
//!     for example an untranscribed voice capture), `scope`, `selection_reason`, `responses`).
//!   * `nothing_to_offer`: no item is eligible any more. It carries no count or backlog.
//!   A notification identifier that is not a daily prompt is `invalid_request`.
//! * `respond` (`command_id`, `item_id`, `expected_revision`, `response`: `not_now` |
//!   `stop_suggesting` | `done`): applies the response to the offered item.
//!   * `applied` (`replayed`): stored; a repeated `command_id` answers `applied` with `replayed`
//!     true and writes nothing.
//!   * `stale_revision` (`current_revision`): the item changed since the user saw it; nothing
//!     was written.
//!   * `no_longer_current`: the item is deleted, completed, cancelled or gone.
//!   * `command_conflict`: `command_id` was already used for a different response.
//!
//! Done completes the item and cancels its own reminder in the same transaction; the next
//! reminder pass removes the pending notification. Not-now and stop-suggesting touch no reminder.
//! Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::export::ReadAuthentication;
use crate::providers::contracts::ErrorClass;
use crate::store::events::{EventError, ItemScope};
use crate::store::schema::{Database, SystemClock};
use crate::suggestions::coordinator::{
    PromptActivation, PromptCoordinator, PromptCoordinatorError, PromptOffer, PromptResponse,
    PromptResponseCommand, ResponseOutcome,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

const fn unauthorized() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Unauthorized,
        code: "suggestion_not_authorized",
        message: Cow::Borrowed("suggestions require an authenticated read session"),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum Command {
    #[serde(rename = "activate_prompt")]
    Activate {
        read_authentication: String,
        notification_id: String,
        #[serde(default)]
        scope: Option<String>,
    },
    #[serde(rename = "respond")]
    Respond {
        read_authentication: String,
        command_id: String,
        item_id: String,
        expected_revision: i64,
        response: String,
    },
}

/// A command that passed validation.
enum Request {
    Activate {
        authentication: ReadAuthentication,
        notification_id: String,
        scope: Option<ItemScope>,
    },
    Respond {
        authentication: ReadAuthentication,
        command: PromptResponseCommand,
    },
}

fn parse_authentication(label: &str) -> Result<ReadAuthentication, AbiFailure> {
    match label {
        "authenticated" => Ok(ReadAuthentication::Authenticated),
        "capture_only" => Ok(ReadAuthentication::CaptureOnly),
        _ => Err(AbiFailure::INVALID_REQUEST),
    }
}

fn parse_response(label: &str) -> Result<PromptResponse, AbiFailure> {
    match label {
        "not_now" => Ok(PromptResponse::NotNow),
        "stop_suggesting" => Ok(PromptResponse::StopSuggesting),
        "done" => Ok(PromptResponse::Done),
        _ => Err(AbiFailure::INVALID_REQUEST),
    }
}

fn response_label(response: PromptResponse) -> &'static str {
    match response {
        PromptResponse::NotNow => "not_now",
        PromptResponse::StopSuggesting => "stop_suggesting",
        PromptResponse::Done => "done",
    }
}

fn acceptable_identifier(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES
}

impl Command {
    /// Refuse malformed commands before anything is queued.
    fn into_request(self) -> Result<Request, AbiFailure> {
        match self {
            Command::Activate {
                read_authentication,
                notification_id,
                scope,
            } => {
                let authentication = parse_authentication(&read_authentication)?;
                if !acceptable_identifier(&notification_id) {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                let scope = scope
                    .map(|label| {
                        ItemScope::from_str(&label).map_err(|_| AbiFailure::INVALID_REQUEST)
                    })
                    .transpose()?;
                Ok(Request::Activate {
                    authentication,
                    notification_id,
                    scope,
                })
            }
            Command::Respond {
                read_authentication,
                command_id,
                item_id,
                expected_revision,
                response,
            } => {
                let authentication = parse_authentication(&read_authentication)?;
                let response = parse_response(&response)?;
                if !acceptable_identifier(&command_id) || !acceptable_identifier(&item_id) {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                let expected_revision =
                    i32::try_from(expected_revision).map_err(|_| AbiFailure::INVALID_REQUEST)?;
                if expected_revision < 0 {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                Ok(Request::Respond {
                    authentication,
                    command: PromptResponseCommand {
                        command_id,
                        item_id,
                        expected_revision,
                        response,
                    },
                })
            }
        }
    }
}

fn event_failure(error: &EventError) -> AbiFailure {
    match error {
        EventError::Invalid(_) | EventError::RevisionMismatch { .. } => AbiFailure::INVALID_REQUEST,
        EventError::Database(error) => AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR),
        EventError::Storage(error) => {
            AbiFailure::from_store(&anyhow::anyhow!("{error}"), AbiFailure::STORAGE_ERROR)
        }
        _ => AbiFailure::STORAGE_ERROR,
    }
}

fn coordinator_failure(error: &PromptCoordinatorError) -> AbiFailure {
    match error {
        PromptCoordinatorError::NotAuthenticated => unauthorized(),
        PromptCoordinatorError::NotAPrompt(_) => AbiFailure::INVALID_REQUEST,
        PromptCoordinatorError::Event(error) => event_failure(error),
        PromptCoordinatorError::CommandConflict { .. }
        | PromptCoordinatorError::Reminder(_)
        | PromptCoordinatorError::Storage(_) => AbiFailure::STORAGE_ERROR,
    }
}

fn offer_answer(offer: &PromptOffer) -> Value {
    json!({
        "outcome": "offer",
        "item_id": offer.item_id,
        "revision": offer.revision,
        "text": offer.text,
        "scope": offer.scope.as_str(),
        "selection_reason": offer.selection_reason,
        "responses": offer.responses.iter().map(|response| response_label(*response)).collect::<Vec<_>>(),
    })
}

/// Runs one request against the store. `Ok` carries the answer body, without `operation_id`.
fn run(database: &mut Database, request: &Request) -> Result<Value, AbiFailure> {
    let clock = SystemClock;
    let coordinator = PromptCoordinator::new(&clock);
    match request {
        Request::Activate {
            authentication,
            notification_id,
            scope,
        } => {
            let activation = coordinator
                .activate(database, *authentication, notification_id, *scope)
                .map_err(|error| coordinator_failure(&error))?;
            Ok(match activation {
                PromptActivation::Offer(offer) => offer_answer(&offer),
                PromptActivation::NothingToOffer => json!({ "outcome": "nothing_to_offer" }),
            })
        }
        Request::Respond {
            authentication,
            command,
        } => match coordinator.respond(database, *authentication, command) {
            Ok(ResponseOutcome::Applied { replayed }) => {
                Ok(json!({ "outcome": "applied", "replayed": replayed }))
            }
            Ok(ResponseOutcome::NoLongerCurrent) => Ok(json!({ "outcome": "no_longer_current" })),
            Ok(ResponseOutcome::Stale { current_revision }) => Ok(json!({
                "outcome": "stale_revision",
                "current_revision": current_revision,
            })),
            Err(PromptCoordinatorError::CommandConflict { .. }) => {
                Ok(json!({ "outcome": "command_conflict" }))
            }
            Err(PromptCoordinatorError::Event(EventError::StaleRevision { current, .. })) => {
                Ok(json!({
                    "outcome": "stale_revision",
                    "current_revision": current.revision,
                }))
            }
            Err(error) => Err(coordinator_failure(&error)),
        },
    }
}

/// Parses one command document and runs it in its own transaction, exactly as the export does
/// on the worker thread. `Ok` is the answer body without `operation_id`.
pub fn apply_suggestion_command(
    database: &mut Database,
    command_json: &[u8],
) -> Result<Value, AbiFailure> {
    let command: Command =
        serde_json::from_slice(command_json).map_err(|_| AbiFailure::INVALID_REQUEST)?;
    run(database, &command.into_request()?)
}

fn suggestion_job(request: Request, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &request)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one suggestion command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields, empty identifiers, unknown labels and
/// out-of-range revisions are refused as `invalid_request` before anything is queued). The
/// outcome event for `operation_id` is the JSON documented in the module documentation, or a
/// normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_suggestion_response(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        if request_len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if request_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if request.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let bytes = std::slice::from_raw_parts(request, request_len);
        let command: Command =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let request = command.into_request()?;
        core.submit(operation_id, suggestion_job(request, operation_id))
    })
}

#[cfg(test)]
mod tests;
