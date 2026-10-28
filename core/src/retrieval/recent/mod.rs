// Newest-first listing of readable items for the home screen (R06).
//
// The listing reads the authoritative tables (items, captures, corrections,
// transcript_attachments) directly, so it needs no search query and cannot disagree with the
// scope rules of retrieval: only open (active) items appear, so completed, cancelled and
// deleted (tombstoned) items never do, and only items whose current scope is in `read_scopes`
// do. Ordering is the capture instant as a point in time
// (so mixed UTC offsets order correctly), newest first, then item id. Paging is by keyset cursor
// rather than offset, so a capture saved between two pages cannot shift or repeat an item.
// Items whose stored capture instant is not a valid timestamp cannot be placed in the order and
// are left out.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::store::events::ItemScope;

/// Where the item's content was captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentSourceKind {
    Text,
    Voice,
}

impl RecentSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecentSourceKind::Text => "text",
            RecentSourceKind::Voice => "voice",
        }
    }
}

/// One row of the recent-items listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentItem {
    pub item_id: String,
    pub revision: i64,
    /// `None` while the item is unclassified.
    pub item_type: Option<String>,
    /// The current text (latest text correction, else the on-device transcript, else the
    /// captured text). `None` for an audio capture with no transcript yet.
    pub display_text: Option<String>,
    pub captured_at: String,
    pub source_kind: RecentSourceKind,
}

/// A page of recent items and the cursor of the next page, if there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentPage {
    pub items: Vec<RecentItem>,
    pub next_cursor: Option<String>,
}

const CURSOR_SEPARATOR: char = '|';

/// Splits a cursor produced by this module into its capture instant and item id.
pub fn parse_cursor(cursor: &str) -> Option<(&str, &str)> {
    let (instant, item_id) = cursor.split_once(CURSOR_SEPARATOR)?;
    if instant.is_empty() || item_id.is_empty() || parse_instant(instant).is_none() {
        return None;
    }
    Some((instant, item_id))
}

fn cursor_for(item: &RecentItem) -> String {
    format!("{}{}{}", item.captured_at, CURSOR_SEPARATOR, item.item_id)
}

fn parse_instant(instant: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(instant)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Up to `limit` readable items in `read_scopes`, newest capture first, starting after `cursor`
/// (a value returned as `next_cursor`, or `None` for the first page). A cursor that does not
/// parse is an error; callers validate it first with [`parse_cursor`].
///
/// The order is computed in Rust on the parsed instant at full sub-second precision: SQLite's
/// `julianday` is only millisecond-accurate and would tie distinct sub-millisecond captures.
pub fn recent_items(
    conn: &Connection,
    read_scopes: &[ItemScope],
    limit: usize,
    cursor: Option<&str>,
) -> Result<RecentPage> {
    if read_scopes.is_empty() || limit == 0 {
        return Ok(RecentPage {
            items: Vec::new(),
            next_cursor: None,
        });
    }
    let cursor_position = match cursor {
        Some(cursor) => {
            let (instant, item_id) = parse_cursor(cursor)
                .ok_or_else(|| anyhow::anyhow!("recent-items cursor is malformed"))?;
            Some((
                parse_instant(instant)
                    .ok_or_else(|| anyhow::anyhow!("recent-items cursor is malformed"))?,
                item_id.to_string(),
            ))
        }
        None => None,
    };
    let scope_placeholders = vec!["?"; read_scopes.len()].join(", ");
    let ordering_sql = format!(
        "SELECT i.item_id, c.capture_instant
           FROM items i
           JOIN captures c ON c.capture_id = i.capture_id
          WHERE i.lifecycle_state = 'active'
            AND COALESCE(i.current_scope, c.item_scope) IN ({scope_placeholders})"
    );
    let scope_parameters = read_scopes.iter().map(|scope| scope.as_str().to_string());
    let mut ordering = conn
        .prepare(&ordering_sql)?
        .query_map(rusqlite::params_from_iter(scope_parameters), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(item_id, instant)| Some((parse_instant(&instant)?, item_id)))
        .filter(|(instant, item_id)| match &cursor_position {
            Some((cursor_instant, cursor_item_id)) => {
                instant < cursor_instant || (instant == cursor_instant && item_id > cursor_item_id)
            }
            None => true,
        })
        .collect::<Vec<_>>();
    ordering.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    ordering.truncate(limit + 1);

    let mut detail = conn.prepare(
        "SELECT i.revision, i.item_type,
                COALESCE(
                    (SELECT t.new_value FROM corrections t
                      WHERE t.item_id = i.item_id AND t.kind = 'text'
                      ORDER BY t.revision DESC LIMIT 1),
                    (SELECT a.text FROM transcript_attachments a WHERE a.item_id = i.item_id),
                    c.text),
                c.capture_instant,
                c.audio_reference IS NOT NULL
           FROM items i
           JOIN captures c ON c.capture_id = i.capture_id
          WHERE i.item_id = ?",
    )?;
    let mut items = Vec::with_capacity(ordering.len());
    for (_, item_id) in ordering {
        let item = detail.query_row([&item_id], |row| {
            let is_voice: bool = row.get(4)?;
            Ok(RecentItem {
                item_id: item_id.clone(),
                revision: row.get(0)?,
                item_type: row.get(1)?,
                display_text: row.get(2)?,
                captured_at: row.get(3)?,
                source_kind: if is_voice {
                    RecentSourceKind::Voice
                } else {
                    RecentSourceKind::Text
                },
            })
        })?;
        items.push(item);
    }
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(cursor_for)
    } else {
        None
    };
    Ok(RecentPage { items, next_cursor })
}
