//! Scoped retrieval over the core handle (R04).
//!
//! `ohand_core_start_retrieval` queues one read on the handle's worker thread and runs the
//! existing `retrieval::query::scoped_query` (text search) or `scoped_list` (filter only). The
//! request is one JSON object with no unknown fields:
//!
//! * `op`: `search` (needs `query`) or `list` (must not carry `query`).
//! * `query`: search text, non-empty after trimming, at most [`OHAND_CORE_MAX_RETRIEVAL_QUERY_BYTES`]
//!   bytes. A query with no searchable character (punctuation only) matches nothing.
//! * `read_scopes`: the scopes the caller is authorized to read, a non-empty list of `personal`
//!   and/or `work`. There is no default: an omitted or empty list is `invalid_request`, and a hit or
//!   count from a scope outside the list is never produced. Native authenticates the session and
//!   passes only the scopes that session may read.
//! * Optional filters: `item_types`, `lifecycle_states`, `route_ids`, `session_topics` (lists),
//!   `include_no_session_topic`, `require_session_topic` (booleans), and `captured_after`,
//!   `captured_before` (RFC 3339, inclusive, `captured_after` not after `captured_before`).
//! * `limit` (1 to [`OHAND_CORE_MAX_RETRIEVAL_LIMIT`], default [`OHAND_CORE_DEFAULT_RETRIEVAL_LIMIT`]) and
//!   `offset` (0 to [`OHAND_CORE_MAX_RETRIEVAL_OFFSET`], default 0) page the stable result order.
//!
//! Anything else (unknown field, op, scope, item type or lifecycle state, an oversized list, a
//! malformed date) is refused as `invalid_request` before anything is queued, or, for dates and
//! ranges, before the store is read. The outcome event is `{"operation_id", "total_accessible",
//! "hits"}`. `total_accessible` counts the items the caller may read that satisfy the filters,
//! before paging. Each hit is `{"item_id", "capture_id", "scope", "text", "text_basis",
//! "original_text", "matched", "match_span", "captured_at"}`: `text` is the current text to show
//! and quote, `text_basis` is `original`, `transcribed` or `corrected`, `original_text` is set
//! only for a corrected item, `matched` is `current` or `superseded_original` (only the replaced
//! original matched), and `match_span` is `{"start","end"}` in Unicode scalar offsets into `text`
//! of the first word that begins with a query term, or `null` for a list or when the match is
//! not in `text`. Failures are normalized and never echo the query, filters or stored text.
//! The query and the results stay on the device: this module sends nothing anywhere.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::retrieval::index::{MatchedText, SearchHit};
use crate::retrieval::query::{scoped_list, scoped_query, QueryFilter, QueryPagination};
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

/// Longest search query accepted, in bytes.
pub const OHAND_CORE_MAX_RETRIEVAL_QUERY_BYTES: usize = 1024;
/// Page size used when the request names no `limit`.
pub const OHAND_CORE_DEFAULT_RETRIEVAL_LIMIT: usize = 25;
/// Largest page a request may ask for.
pub const OHAND_CORE_MAX_RETRIEVAL_LIMIT: usize = 100;
/// Largest `offset` a request may ask for.
pub const OHAND_CORE_MAX_RETRIEVAL_OFFSET: usize = 1_000_000;

const MAX_FILTER_VALUES: usize = 32;
const MAX_FILTER_VALUE_BYTES: usize = 256;
const ITEM_TYPES: [&str; 4] = ["broad_intention", "idea", "note", "action"];
const LIFECYCLE_STATES: [&str; 4] = ["active", "completed", "cancelled", "deleted"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    op: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    read_scopes: Vec<String>,
    #[serde(default)]
    item_types: Vec<String>,
    #[serde(default)]
    lifecycle_states: Vec<String>,
    #[serde(default)]
    route_ids: Vec<String>,
    #[serde(default)]
    session_topics: Vec<String>,
    #[serde(default)]
    include_no_session_topic: bool,
    #[serde(default)]
    require_session_topic: bool,
    #[serde(default)]
    captured_after: Option<String>,
    #[serde(default)]
    captured_before: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

struct Validated {
    search_text: Option<String>,
    filter: QueryFilter,
    pagination: QueryPagination,
}

fn bounded_values(values: &[String]) -> bool {
    values.len() <= MAX_FILTER_VALUES
        && values
            .iter()
            .all(|value| !value.is_empty() && value.len() <= MAX_FILTER_VALUE_BYTES)
}

fn within_vocabulary(values: &[String], vocabulary: &[&str]) -> bool {
    values
        .iter()
        .all(|value| vocabulary.contains(&value.as_str()))
}

fn parse_instant(value: &Option<String>) -> Result<Option<DateTime<Utc>>, AbiFailure> {
    value
        .as_deref()
        .map(|text| {
            DateTime::parse_from_rfc3339(text)
                .map(|instant| instant.with_timezone(&Utc))
                .map_err(|_| AbiFailure::INVALID_REQUEST)
        })
        .transpose()
}

impl Command {
    fn validate(self) -> Result<Validated, AbiFailure> {
        let search_text = match (self.op.as_str(), self.query) {
            ("search", Some(query))
                if !query.trim().is_empty()
                    && query.len() <= OHAND_CORE_MAX_RETRIEVAL_QUERY_BYTES =>
            {
                Some(query)
            }
            ("list", None) => None,
            _ => return Err(AbiFailure::INVALID_REQUEST),
        };

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

        if !bounded_values(&self.item_types)
            || !within_vocabulary(&self.item_types, &ITEM_TYPES)
            || !bounded_values(&self.lifecycle_states)
            || !within_vocabulary(&self.lifecycle_states, &LIFECYCLE_STATES)
            || !bounded_values(&self.route_ids)
            || !bounded_values(&self.session_topics)
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }

        let captured_after = parse_instant(&self.captured_after)?;
        let captured_before = parse_instant(&self.captured_before)?;
        if let (Some(after), Some(before)) = (captured_after, captured_before) {
            if after > before {
                return Err(AbiFailure::INVALID_REQUEST);
            }
        }

        let limit = self.limit.unwrap_or(OHAND_CORE_DEFAULT_RETRIEVAL_LIMIT);
        let offset = self.offset.unwrap_or(0);
        if limit == 0
            || limit > OHAND_CORE_MAX_RETRIEVAL_LIMIT
            || offset > OHAND_CORE_MAX_RETRIEVAL_OFFSET
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }

        Ok(Validated {
            search_text,
            filter: QueryFilter {
                item_types: self.item_types,
                lifecycle_states: self.lifecycle_states,
                route_ids: self.route_ids,
                session_topics: self.session_topics,
                include_no_session_topic: self.include_no_session_topic,
                require_session_topic: self.require_session_topic,
                captured_after: self.captured_after,
                captured_before: self.captured_before,
                read_scopes,
            },
            pagination: QueryPagination { limit, offset },
        })
    }
}

/// Unicode scalar offsets `[start, end)` of the first word in `text` that begins with one of the
/// query's words, compared case-insensitively. Words are maximal alphanumeric runs.
fn match_span(text: &str, query: &str) -> Option<Value> {
    let query_words: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    if query_words.is_empty() {
        return None;
    }
    let mut word_start: Option<usize> = None;
    let mut word = String::new();
    for (position, character) in text.chars().chain(std::iter::once(' ')).enumerate() {
        if character.is_alphanumeric() {
            word_start.get_or_insert(position);
            word.push(character);
            continue;
        }
        if let Some(start) = word_start.take() {
            let lowered = word.to_lowercase();
            if query_words
                .iter()
                .any(|query_word| lowered.starts_with(query_word.as_str()))
            {
                return Some(json!({ "start": start, "end": position }));
            }
            word.clear();
        }
    }
    None
}

fn capture_instant(connection: &Connection, item_id: &str) -> Result<String, AbiFailure> {
    connection
        .query_row(
            "SELECT c.capture_instant FROM items i JOIN captures c ON c.capture_id = i.capture_id
              WHERE i.item_id = ?",
            [item_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| {
            AbiFailure::from_store(&anyhow::Error::new(error), AbiFailure::STORAGE_ERROR)
        })
}

fn hit_json(
    connection: &Connection,
    hit: &SearchHit,
    search_text: Option<&str>,
) -> Result<Value, AbiFailure> {
    let corrected = hit.text_basis.as_str() == "corrected";
    let span = match (search_text, hit.matched) {
        (Some(query), MatchedText::Current) => match_span(&hit.current_text, query),
        _ => None,
    };
    Ok(json!({
        "item_id": hit.item_id,
        "capture_id": hit.capture_id,
        "scope": hit.scope.as_str(),
        "text": hit.current_text,
        "text_basis": hit.text_basis.as_str(),
        "original_text": if corrected { hit.original_text.clone() } else { None },
        "matched": match hit.matched {
            MatchedText::Current => "current",
            MatchedText::SupersededOriginal => "superseded_original",
        },
        "match_span": span,
        "captured_at": capture_instant(connection, &hit.item_id)?,
    }))
}

fn run(database: &Database, request: &Validated) -> Result<Value, AbiFailure> {
    let connection = database.conn();
    let result = match &request.search_text {
        Some(query) => scoped_query(connection, query, &request.filter, &request.pagination),
        None => scoped_list(connection, &request.filter, &request.pagination),
    }
    .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR))?;
    let hits = result
        .hits
        .iter()
        .map(|hit| hit_json(connection, hit, request.search_text.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "total_accessible": result.total_accessible, "hits": hits }))
}

fn retrieval_job(request: Validated, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let mut body = run(&database, &request)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one scoped retrieval described by the `request_len` JSON bytes at `request` (see the
/// module documentation). A malformed or out-of-bounds request, including one that names no read
/// scope, is refused as `invalid_request` before anything is queued. Native must have
/// authenticated the session before it calls this and passes only scopes that session may read.
/// The outcome event for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_retrieval(
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
            retrieval_job(command.validate()?, operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
