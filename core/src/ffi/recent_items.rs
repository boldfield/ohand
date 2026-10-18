//! The most recent captures, for the home screen, over the core handle (R06).
//!
//! `ohand_core_start_recent_items` queues one read on the handle's worker thread. The request is
//! one JSON object with no unknown fields:
//!
//! * `read_scopes`: the scopes the caller is authorized to read, a non-empty list of `personal`
//!   and/or `work`. There is no default: an omitted or empty list is `invalid_request`, and an
//!   item outside the list is never returned. Native authenticates the session and passes only
//!   the scopes that session may read.
//! * `limit` (1 to [`OHAND_CORE_MAX_RECENT_ITEMS_LIMIT`], default
//!   [`OHAND_CORE_DEFAULT_RECENT_ITEMS_LIMIT`]): the page size.
//! * `cursor`: the `next_cursor` of the previous page, absent for the first page. It is opaque to
//!   the caller; a malformed one is `invalid_request`.
//!
//! The outcome event is `{"operation_id", "items", "next_cursor"}`. `items` is newest capture
//! first, then item id. `next_cursor` is `null` on the last page. Each item is `{"item_id",
//! "revision", "item_type", "display_text", "captured_at", "source_kind"}`: `revision` is the
//! item's current revision, `item_type` is `null` while the item is unclassified,
//! `display_text` is the interpreted title when the item has one and otherwise the item's current
//! source text (latest text correction, else the on-device transcript, else the captured text; the
//! core records no interpreted title yet, so today it is always the source text) and is `null`
//! for an audio capture with no transcript yet, `captured_at` is the capture instant as stored
//! (RFC 3339) and `source_kind` is `text`, or `voice` for a capture with audio.
//!
//! The scope and deletion rules are retrieval's: a deleted (tombstoned) item never appears and a
//! work-only read never returns a personal item. Failures are normalized and never echo the
//! cursor or stored content. The listing stays on the device: this module sends nothing anywhere.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::retrieval::recent::{parse_cursor, recent_items};
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

/// Page size used when the request names no `limit`.
pub const OHAND_CORE_DEFAULT_RECENT_ITEMS_LIMIT: usize = 30;
/// Largest page a request may ask for.
pub const OHAND_CORE_MAX_RECENT_ITEMS_LIMIT: usize = 100;
/// Longest cursor accepted, in bytes.
pub const OHAND_CORE_MAX_RECENT_ITEMS_CURSOR_BYTES: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    #[serde(default)]
    read_scopes: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

struct Validated {
    read_scopes: Vec<ItemScope>,
    limit: usize,
    cursor: Option<String>,
}

impl Command {
    fn validate(self) -> Result<Validated, AbiFailure> {
        let mut read_scopes: Vec<ItemScope> = Vec::new();
        for scope in &self.read_scopes {
            let scope = ItemScope::from_str(scope).map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if !read_scopes.contains(&scope) {
                read_scopes.push(scope);
            }
        }
        if read_scopes.is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let limit = self.limit.unwrap_or(OHAND_CORE_DEFAULT_RECENT_ITEMS_LIMIT);
        if limit == 0 || limit > OHAND_CORE_MAX_RECENT_ITEMS_LIMIT {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if let Some(cursor) = &self.cursor {
            if cursor.len() > OHAND_CORE_MAX_RECENT_ITEMS_CURSOR_BYTES
                || parse_cursor(cursor).is_none()
            {
                return Err(AbiFailure::INVALID_REQUEST);
            }
        }
        Ok(Validated {
            read_scopes,
            limit,
            cursor: self.cursor,
        })
    }
}

fn read_recent_items(database: &Database, request: &Validated) -> Result<Value, AbiFailure> {
    let page = recent_items(
        database.conn(),
        &request.read_scopes,
        request.limit,
        request.cursor.as_deref(),
    )
    .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR))?;
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|item| {
            json!({
                "item_id": item.item_id,
                "revision": item.revision,
                "item_type": item.item_type,
                "display_text": item.display_text,
                "captured_at": item.captured_at,
                "source_kind": item.source_kind.as_str(),
            })
        })
        .collect();
    Ok(json!({ "items": items, "next_cursor": page.next_cursor }))
}

fn recent_items_job(request: Validated, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let mut body = read_recent_items(&database, &request)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues the read of recent items described by the `request_len` JSON bytes at `request` (see
/// the module documentation). A malformed request, including one that names no read scope, is
/// refused as `invalid_request` before anything is queued. Native must have authenticated the
/// session before it calls this and passes only scopes that session may read. The outcome event
/// for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_recent_items(
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
        core.submit(
            operation_id,
            recent_items_job(command.validate()?, operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
