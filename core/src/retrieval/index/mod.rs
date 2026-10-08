// Full-text search indexing for original and corrected text.
//
// Maintains a transactional FTS5 index of original captures and user corrections.
// Insert/correction/deletion hooks keep the index in sync; crash/rebuild cannot invent or
// resurrect records. The index enforces privacy scope and does not present derived model
// summaries as original quotations.

use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction};

/// Rebuild the FTS index from authoritative source records.
/// Idempotent: safe to call multiple times. Verifies that every indexed record corresponds
/// to an accessible capture; crash/rebuild cannot invent records.
pub fn rebuild_index(tx: &Transaction<'_>) -> Result<()> {
    // Clear the FTS index completely for a clean rebuild.
    tx.execute("DELETE FROM search_index", [])?;

    // Populate FTS index from captures.
    // Only include records that are:
    // - Not deleted (accessible)
    // - Have searchable text (original)
    // Privacy scope uses effective scope: COALESCE(items.current_scope, captures.item_scope).
    tx.execute(
        "INSERT INTO search_index (item_id, capture_id, item_scope, original_text, text_basis)
         SELECT i.item_id, i.capture_id, COALESCE(i.current_scope, c.item_scope), c.text, 'original'
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         WHERE i.lifecycle_state != 'deleted' AND c.text IS NOT NULL AND c.text != ''",
        [],
    )?;

    // Add corrected text entries. User corrections override original text in the current_text column.
    // Each item should have at most one current_text entry (the latest correction).
    tx.execute(
        "INSERT INTO search_index (item_id, capture_id, item_scope, current_text, text_basis)
         SELECT DISTINCT i.item_id, i.capture_id, COALESCE(i.current_scope, c.item_scope), corr.new_value, 'corrected'
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         JOIN corrections corr ON i.item_id = corr.item_id
         WHERE i.lifecycle_state != 'deleted'
           AND corr.kind = 'text'
           AND corr.new_value IS NOT NULL
           AND corr.new_value != ''
           AND corr.revision = (
               SELECT MAX(revision)
               FROM corrections cor2
               WHERE cor2.item_id = i.item_id AND cor2.kind = 'text'
           )",
        [],
    )?;

    Ok(())
}

/// Index a newly captured item's original text.
/// Called transactionally when a capture is first stored.
/// Idempotent: safe to call multiple times for the same item.
/// Skips indexing if the item has been deleted to prevent resurrecting deleted content on retry.
pub fn index_capture(tx: &Transaction<'_>, item_id: &str, capture_id: &str) -> Result<()> {
    // Check if the item is deleted; don't index deleted items to prevent retry resurrection.
    let is_deleted: bool = tx
        .query_row(
            "SELECT lifecycle_state = 'deleted' FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);

    if is_deleted {
        return Ok(());
    }

    // Fetch the capture record and effective item scope.
    let (text, capture_scope, current_scope): (Option<String>, String, Option<String>) = tx
        .query_row(
            "SELECT c.text, c.item_scope, i.current_scope
             FROM captures c
             JOIN items i ON i.item_id = ?
             WHERE c.capture_id = ?",
            rusqlite::params![item_id, capture_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

    let effective_scope = current_scope.unwrap_or(capture_scope);

    // Index only if text is present and non-empty.
    if let Some(text) = text {
        if !text.is_empty() {
            // Check if we already indexed this item's original text (idempotency).
            let existing: Option<i64> = tx.query_row(
                "SELECT COUNT(*) FROM search_index WHERE item_id = ? AND text_basis = 'original'",
                [item_id],
                |row| row.get(0),
            ).optional()?;

            if existing.unwrap_or(0) == 0 {
                tx.execute(
                    "INSERT INTO search_index (item_id, capture_id, item_scope, original_text, text_basis)
                     VALUES (?, ?, ?, ?, 'original')",
                    rusqlite::params![item_id, capture_id, effective_scope, text],
                )?;
            }
        }
    }

    Ok(())
}

/// Index a text correction.
/// Called transactionally when a user corrects an item's text.
/// Idempotent: replaces the previous correction while keeping the original (for incremental and rebuild convergence).
pub fn index_text_correction(tx: &Transaction<'_>, item_id: &str) -> Result<()> {
    // Fetch the latest user correction, if it exists.
    let new_text: Option<String> = tx
        .query_row(
            "SELECT new_value FROM corrections
         WHERE item_id = ? AND kind = 'text'
         ORDER BY revision DESC LIMIT 1",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;

    // Delete only the previous corrected entry, keep the original.
    // This ensures incremental and rebuild agree: both keep [original, latest_correction].
    tx.execute(
        "DELETE FROM search_index WHERE item_id = ? AND text_basis = 'corrected'",
        [item_id],
    )?;

    // Index the new corrected text if it's non-empty.
    if let Some(text) = new_text {
        if !text.is_empty() {
            // Get capture_id and effective item scope from items and captures.
            let (capture_id, capture_scope, current_scope): (String, String, Option<String>) = tx
                .query_row(
                "SELECT i.capture_id, c.item_scope, i.current_scope FROM items i
                JOIN captures c ON i.capture_id = c.capture_id
                WHERE i.item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;

            let effective_scope = current_scope.unwrap_or(capture_scope);

            tx.execute(
                "INSERT INTO search_index (item_id, capture_id, item_scope, current_text, text_basis)
                 VALUES (?, ?, ?, ?, 'corrected')",
                rusqlite::params![item_id, capture_id, effective_scope, text],
            )?;
        } else {
            // Empty correction: keep only the original, don't add an empty corrected row.
        }
    }

    Ok(())
}

/// Remove all index entries for a deleted item.
/// Called transactionally when an item is marked as deleted.
/// Idempotent: safe to call multiple times for the same item.
pub fn remove_item_from_index(tx: &Transaction<'_>, item_id: &str) -> Result<()> {
    tx.execute("DELETE FROM search_index WHERE item_id = ?", [item_id])?;
    Ok(())
}

/// Query the index for searchable items.
/// Returns items matching the query text across both original and corrected text.
/// Results explicitly identify whether text is original or corrected, and include
/// capture_id and item_scope for source attribution and privacy filtering.
pub fn search_index(tx: &Transaction<'_>, query: &str) -> Result<Vec<SearchResult>> {
    let mut stmt = tx.prepare(
        "SELECT item_id, capture_id, item_scope, original_text, current_text, text_basis
         FROM search_index
         WHERE original_text MATCH ? OR current_text MATCH ?
         ORDER BY rank",
    )?;

    let results = stmt
        .query_map(rusqlite::params![query, query], |row| {
            Ok(SearchResult {
                item_id: row.get(0)?,
                capture_id: row.get(1)?,
                item_scope: row.get(2)?,
                original_text: row.get(3)?,
                corrected_text: row.get(4)?,
                source_type: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// A search result from the FTS index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    /// The item_id that matches the search.
    pub item_id: String,
    /// The capture_id for source attribution.
    pub capture_id: String,
    /// The item_scope (privacy classification) of the source capture.
    pub item_scope: String,
    /// Original capture text (if indexed).
    pub original_text: Option<String>,
    /// User-corrected text (if indexed).
    pub corrected_text: Option<String>,
    /// Either 'original' or 'corrected' to indicate which text matched.
    pub source_type: String,
}

impl SearchResult {
    /// Get the text that should be displayed (corrected if available, else original).
    /// This ensures corrected text takes precedence, preventing model summaries
    /// from being presented as original quotations.
    pub fn display_text(&self) -> Option<&str> {
        self.corrected_text
            .as_deref()
            .or(self.original_text.as_deref())
    }
}

/// Query the source (captures and corrections) directly, bypassing the FTS index.
/// Provides an authoritative fallback for comparison with index results.
/// Uses case-insensitive substring matching (LIKE) with proper escaping.
/// Only returns accessible (non-deleted) items.
/// Privacy scope uses effective scope: COALESCE(items.current_scope, captures.item_scope).
pub fn search_source_direct(tx: &Transaction<'_>, query: &str) -> Result<Vec<SearchResult>> {
    // Escape LIKE special characters (% and _) to ensure literal matching.
    let escaped_query = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{}%", escaped_query);

    // Query captures for original text matching the query pattern.
    let mut stmt = tx.prepare(
        "SELECT DISTINCT i.item_id, i.capture_id, COALESCE(i.current_scope, c.item_scope) as item_scope, c.text as text, NULL as corrected_text, 'original' as source_type
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         WHERE i.lifecycle_state != 'deleted'
           AND c.text IS NOT NULL
           AND c.text LIKE ? ESCAPE '\\'
         UNION
         SELECT DISTINCT i.item_id, i.capture_id, COALESCE(i.current_scope, c.item_scope) as item_scope, NULL as text, corr.new_value as corrected_text, 'corrected' as source_type
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         JOIN corrections corr ON i.item_id = corr.item_id
         WHERE i.lifecycle_state != 'deleted'
           AND corr.kind = 'text'
           AND corr.new_value IS NOT NULL
           AND corr.new_value LIKE ? ESCAPE '\\'
           AND corr.revision = (
               SELECT MAX(revision)
               FROM corrections cor2
               WHERE cor2.item_id = i.item_id AND cor2.kind = 'text'
           )
         ORDER BY item_id, source_type DESC",
    )?;

    let results = stmt
        .query_map(rusqlite::params![&pattern, &pattern], |row| {
            Ok(SearchResult {
                item_id: row.get(0)?,
                capture_id: row.get(1)?,
                item_scope: row.get(2)?,
                original_text: row.get(3)?,
                corrected_text: row.get(4)?,
                source_type: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// Verify index integrity: all indexed items exist and have accessible captures.
/// Used to detect stale or corrupted index entries.
pub fn verify_index_integrity(tx: &Transaction<'_>) -> Result<IndexIntegrityIssue> {
    // Check for orphaned index entries (item_id with no corresponding item).
    let orphaned_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM search_index si
         WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.item_id = si.item_id)",
        [],
        |row| row.get(0),
    )?;

    if orphaned_count > 0 {
        return Ok(IndexIntegrityIssue::OrphanedEntries(orphaned_count));
    }

    // Check for missing index entries: accessible items with text that aren't indexed.
    // An item should be indexed if it has original or corrected text and is not deleted.
    let missing_count: i64 = tx.query_row(
        "SELECT COUNT(DISTINCT i.item_id)
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         WHERE i.lifecycle_state != 'deleted'
           AND (c.text IS NOT NULL AND c.text != ''
                OR EXISTS (SELECT 1 FROM corrections cor
                           WHERE cor.item_id = i.item_id
                           AND cor.kind = 'text'
                           AND cor.new_value IS NOT NULL
                           AND cor.new_value != ''))
           AND NOT EXISTS (SELECT 1 FROM search_index si WHERE si.item_id = i.item_id)",
        [],
        |row| row.get(0),
    )?;

    if missing_count > 0 {
        return Ok(IndexIntegrityIssue::MissingEntries(missing_count));
    }

    Ok(IndexIntegrityIssue::Healthy)
}

/// Issues detected during index integrity verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexIntegrityIssue {
    /// Index is healthy.
    Healthy,
    /// Orphaned entries in the index (items that no longer exist).
    OrphanedEntries(i64),
    /// Missing entries in the index (accessible items not indexed).
    MissingEntries(i64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_result_display_text_prioritizes_corrected() {
        let result = SearchResult {
            item_id: "item1".to_string(),
            capture_id: "cap1".to_string(),
            item_scope: "personal".to_string(),
            original_text: Some("original".to_string()),
            corrected_text: Some("corrected".to_string()),
            source_type: "corrected".to_string(),
        };

        // Corrected text takes precedence, preventing model summaries from being shown as original.
        assert_eq!(result.display_text(), Some("corrected"));
    }

    #[test]
    fn test_search_result_display_text_falls_back_to_original() {
        let result = SearchResult {
            item_id: "item2".to_string(),
            capture_id: "cap2".to_string(),
            item_scope: "work".to_string(),
            original_text: Some("original".to_string()),
            corrected_text: None,
            source_type: "original".to_string(),
        };

        // Falls back to original if no correction exists.
        assert_eq!(result.display_text(), Some("original"));
    }

    #[test]
    fn test_search_result_display_text_handles_empty() {
        let result = SearchResult {
            item_id: "item3".to_string(),
            capture_id: "cap3".to_string(),
            item_scope: "personal".to_string(),
            original_text: None,
            corrected_text: None,
            source_type: "original".to_string(),
        };

        // Returns None when no text is available.
        assert_eq!(result.display_text(), None);
    }
}
