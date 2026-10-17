// Newest-first listing of readable items for the home screen (R06).
//
// The listing reads the authoritative tables (items, captures, corrections,
// transcript_attachments) directly, so it needs no search query and cannot disagree with the
// scope and lifecycle rules of retrieval: deleted (tombstoned) items never appear and only items
// whose current scope is in `read_scopes` do. Ordering is the capture instant as a point in time
// (so mixed UTC offsets order correctly), newest first, then item id. Paging is by keyset cursor
// rather than offset, so a capture saved between two pages cannot shift or repeat an item.
// Items whose stored capture instant is not a valid timestamp cannot be placed in the order and
// are left out.

use anyhow::Result;
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
    if instant.is_empty() || item_id.is_empty() {
        return None;
    }
    Some((instant, item_id))
}

fn cursor_for(item: &RecentItem) -> String {
    format!("{}{}{}", item.captured_at, CURSOR_SEPARATOR, item.item_id)
}

/// Up to `limit` readable items in `read_scopes`, newest capture first, starting after `cursor`
/// (a value returned as `next_cursor`, or `None` for the first page). A cursor that does not
/// parse is an error; callers validate it first with [`parse_cursor`].
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
    let scope_placeholders = vec!["?"; read_scopes.len()].join(", ");
    let cursor_clause = if cursor.is_some() {
        "AND (julianday(c.capture_instant) < julianday(?)
              OR (julianday(c.capture_instant) = julianday(?) AND i.item_id > ?))"
    } else {
        ""
    };
    let sql = format!(
        "SELECT i.item_id, i.revision, i.item_type,
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
          WHERE i.lifecycle_state != 'deleted'
            AND COALESCE(i.current_scope, c.item_scope) IN ({scope_placeholders})
            AND julianday(c.capture_instant) IS NOT NULL
            {cursor_clause}
          ORDER BY julianday(c.capture_instant) DESC, i.item_id ASC
          LIMIT ?"
    );
    let mut parameters: Vec<rusqlite::types::Value> = read_scopes
        .iter()
        .map(|scope| scope.as_str().to_string().into())
        .collect();
    if let Some(cursor) = cursor {
        let (instant, item_id) = parse_cursor(cursor)
            .ok_or_else(|| anyhow::anyhow!("recent-items cursor is malformed"))?;
        parameters.push(instant.to_string().into());
        parameters.push(instant.to_string().into());
        parameters.push(item_id.to_string().into());
    }
    parameters.push(((limit + 1) as i64).into());

    let mut statement = conn.prepare(&sql)?;
    let mut items = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            let is_voice: bool = row.get(5)?;
            Ok(RecentItem {
                item_id: row.get(0)?,
                revision: row.get(1)?,
                item_type: row.get(2)?,
                display_text: row.get(3)?,
                captured_at: row.get(4)?,
                source_kind: if is_voice {
                    RecentSourceKind::Voice
                } else {
                    RecentSourceKind::Text
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(cursor_for)
    } else {
        None
    };
    Ok(RecentPage { items, next_cursor })
}
