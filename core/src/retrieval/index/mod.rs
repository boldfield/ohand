// Full-text index over original and explicitly corrected text.
// Implementation owned by R01.
//
// `search_index` is a derived projection with exactly one row per readable item:
// the original capture text, the effective current text (latest explicit text correction,
// else the original) and which of the two is current. Only user-authored text is indexed;
// model proposals, summaries and other derived values never enter it.
//
// Every write goes through `sync_item_in_tx`, which re-reads the authoritative tables
// (items, captures, corrections) and replaces the item's row, so the hooks are idempotent,
// order-independent and converge on exactly what `rebuild_index` produces. Lifecycle and
// privacy scope are never copied into the index: searches join `items`/`captures` at query
// time, so a stale or orphaned index row can neither resurrect a deleted item nor leak text
// across an explicitly corrected scope.
//
// Query contract (shared by the index and the direct-source fallback): the query is split on
// whitespace; every piece containing a letter or digit becomes a quoted FTS5 phrase prefix
// term, so FTS operators and quote characters are plain text; a record matches when all terms
// match as token prefixes within its original text, or within its current text.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, Transaction};
use std::collections::HashSet;

use crate::store::events::ItemScope;

const SCRATCH_TABLE: &str = "search_source_scan";

/// Selects the rows the index must hold, straight from the authoritative tables. Parameter 1
/// optionally restricts the projection to a single item.
const PROJECTION_SQL: &str = "
    SELECT i.item_id,
           c.text,
           COALESCE(
               (SELECT t.new_value FROM corrections t
                 WHERE t.item_id = i.item_id AND t.kind = 'text'
                 ORDER BY t.revision DESC LIMIT 1),
               c.text),
           CASE WHEN EXISTS (SELECT 1 FROM corrections t
                              WHERE t.item_id = i.item_id AND t.kind = 'text')
                THEN 'corrected' ELSE 'original' END
      FROM items i
      JOIN captures c ON c.capture_id = i.capture_id
     WHERE i.lifecycle_state != 'deleted'
       AND (?1 IS NULL OR i.item_id = ?1)
       AND (c.text IS NOT NULL
            OR EXISTS (SELECT 1 FROM corrections t
                        WHERE t.item_id = i.item_id AND t.kind = 'text'))";

/// Which text the current value of an item comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextBasis {
    /// The text as captured.
    Original,
    /// The latest explicit user text correction.
    Corrected,
}

impl TextBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            TextBasis::Original => "original",
            TextBasis::Corrected => "corrected",
        }
    }

    fn parse(stored: &str) -> Result<Self> {
        match stored {
            "original" => Ok(TextBasis::Original),
            "corrected" => Ok(TextBasis::Corrected),
            other => Err(anyhow!("Unknown text basis in search index: {other}")),
        }
    }
}

/// Which stored text a query matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchedText {
    /// The current effective text matched.
    Current,
    /// Only the original text matched, and it has since been superseded by a correction.
    /// Callers must show the current text and must not present the original as current.
    SupersededOriginal,
}

/// Outcome of resynchronising one item's index row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexChange {
    Indexed,
    Removed,
}

/// A source-attributed text match. Quotations are always user-authored text, never model output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub item_id: String,
    pub capture_id: String,
    /// Effective privacy scope: an explicit scope correction, else the capture-time scope.
    pub scope: ItemScope,
    pub route_id: String,
    pub lifecycle_state: String,
    /// Current effective text; this is what to display and quote.
    pub current_text: String,
    pub text_basis: TextBasis,
    pub original_text: Option<String>,
    pub matched: MatchedText,
}

/// Recompute the index row of one item from the authoritative tables, inside the caller's
/// transaction. Idempotent: it removes the item's row and inserts the projection when the item
/// exists, is not deleted and has text. Call it after any change to the item's text or lifecycle.
pub fn sync_item_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<IndexChange> {
    tx.execute("DELETE FROM search_index WHERE item_id = ?", [item_id])?;
    let inserted = tx.execute(
        &format!(
            "INSERT INTO search_index (item_id, original_text, current_text, text_basis)
             {PROJECTION_SQL}"
        ),
        [item_id],
    )?;
    Ok(if inserted > 0 {
        IndexChange::Indexed
    } else {
        IndexChange::Removed
    })
}

/// Drop an item's index row (deletion hook). Retrying `sync_item_in_tx` afterwards cannot
/// restore it, because the projection excludes deleted items.
pub fn remove_item_from_index(tx: &Transaction<'_>, item_id: &str) -> Result<usize> {
    Ok(tx.execute("DELETE FROM search_index WHERE item_id = ?", [item_id])?)
}

/// Discard the index and rebuild it from the authoritative tables. Returns the row count.
pub fn rebuild_index(tx: &Transaction<'_>) -> Result<usize> {
    tx.execute("DELETE FROM search_index", [])?;
    Ok(tx.execute(
        &format!(
            "INSERT INTO search_index (item_id, original_text, current_text, text_basis)
             {PROJECTION_SQL}"
        ),
        [rusqlite::types::Null],
    )?)
}

/// Build the shared MATCH expression, or `None` when the query has no searchable term.
fn match_terms(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|piece| piece.chars().any(char::is_alphanumeric))
        .map(|piece| format!("\"{}\"*", piece.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Search the full-text index. Only items readable under `allowed_scopes` (by effective scope,
/// evaluated before ranking) and not deleted are returned, best match first.
pub fn search_index(
    conn: &Connection,
    query: &str,
    allowed_scopes: &[ItemScope],
) -> Result<Vec<SearchHit>> {
    search_table(conn, "search_index", query, allowed_scopes)
}

/// Answer the same query without reading `search_index`: the projection is evaluated straight
/// from the authoritative tables into a throwaway temp table that uses the same tokenizer, so the
/// fallback agrees with a freshly rebuilt index on every accessible record. Results are ordered
/// by item id.
pub fn search_source_direct(
    conn: &Connection,
    query: &str,
    allowed_scopes: &[ItemScope],
) -> Result<Vec<SearchHit>> {
    if match_terms(query).is_none() || allowed_scopes.is_empty() {
        return Ok(Vec::new());
    }
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.{SCRATCH_TABLE};
         CREATE VIRTUAL TABLE temp.{SCRATCH_TABLE} USING fts5(
             item_id UNINDEXED, original_text, current_text, text_basis)"
    ))?;
    let outcome = (|| {
        conn.execute(
            &format!(
                "INSERT INTO {SCRATCH_TABLE} (item_id, original_text, current_text, text_basis)
                 {PROJECTION_SQL}"
            ),
            [rusqlite::types::Null],
        )?;
        let mut hits = search_table(conn, SCRATCH_TABLE, query, allowed_scopes)?;
        hits.sort_by(|a, b| a.item_id.cmp(&b.item_id));
        Ok(hits)
    })();
    conn.execute_batch(&format!("DROP TABLE IF EXISTS temp.{SCRATCH_TABLE}"))?;
    outcome
}

fn search_table(
    conn: &Connection,
    table: &str,
    query: &str,
    allowed_scopes: &[ItemScope],
) -> Result<Vec<SearchHit>> {
    let Some(terms) = match_terms(query) else {
        return Ok(Vec::new());
    };
    if allowed_scopes.is_empty() {
        return Ok(Vec::new());
    }
    let either_text = format!("(original_text : ({terms})) OR (current_text : ({terms}))");
    let current_only = format!("current_text : ({terms})");

    let scope_placeholders = vec!["?"; allowed_scopes.len()].join(", ");
    let sql = format!(
        "SELECT {table}.item_id, i.capture_id, COALESCE(i.current_scope, c.item_scope),
                c.route_id, i.lifecycle_state, {table}.current_text, {table}.text_basis,
                {table}.original_text
           FROM {table}
           JOIN items i ON i.item_id = {table}.item_id
           JOIN captures c ON c.capture_id = i.capture_id
          WHERE {table} MATCH ?
            AND i.lifecycle_state != 'deleted'
            AND COALESCE(i.current_scope, c.item_scope) IN ({scope_placeholders})
          ORDER BY rank, {table}.item_id"
    );
    let mut params: Vec<String> = vec![either_text];
    params.extend(
        allowed_scopes
            .iter()
            .map(|scope| scope.as_str().to_string()),
    );

    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut current_matches = HashSet::new();
    let mut current_statement = conn.prepare(&format!(
        "SELECT item_id FROM {table} WHERE {table} MATCH ?"
    ))?;
    for item_id in current_statement.query_map([&current_only], |row| row.get::<_, String>(0))? {
        current_matches.insert(item_id?);
    }

    rows.into_iter()
        .map(
            |(
                item_id,
                capture_id,
                scope,
                route_id,
                lifecycle_state,
                current_text,
                text_basis,
                original_text,
            )| {
                let matched = if current_matches.contains(&item_id) {
                    MatchedText::Current
                } else {
                    MatchedText::SupersededOriginal
                };
                Ok(SearchHit {
                    item_id,
                    capture_id,
                    scope: scope.parse::<ItemScope>()?,
                    route_id,
                    lifecycle_state,
                    current_text: current_text.unwrap_or_default(),
                    text_basis: TextBasis::parse(&text_basis)?,
                    original_text,
                    matched,
                })
            },
        )
        .collect()
}
