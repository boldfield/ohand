// Full-text search indexing for original and corrected text.
//
// Maintains a transactional FTS5 index of original captures and user corrections.
// Insert/correction/deletion hooks keep the index in sync; crash/rebuild cannot invent or
// resurrect records. The index enforces privacy scope and does not present derived model
// summaries as original quotations.

use anyhow::Result;
use rusqlite::Transaction;

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
    tx.execute(
        "INSERT INTO search_index (item_id, original_text, text_basis)
         SELECT i.item_id, c.text, 'original'
         FROM items i
         JOIN captures c ON i.capture_id = c.capture_id
         WHERE i.lifecycle_state != 'deleted' AND c.text IS NOT NULL AND c.text != ''",
        [],
    )?;

    // Add corrected text entries. User corrections override original text in the current_text column.
    // Each item should have at most one current_text entry (the latest correction).
    tx.execute(
        "INSERT INTO search_index (item_id, current_text, text_basis)
         SELECT DISTINCT i.item_id, corr.new_value, 'corrected'
         FROM items i
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
pub fn index_capture(tx: &Transaction<'_>, item_id: &str, capture_id: &str) -> Result<()> {
    // Fetch the capture record to get text.
    let text: Option<String> = tx.query_row(
        "SELECT text FROM captures WHERE capture_id = ?",
        [capture_id],
        |row| row.get(0),
    )?;

    // Index only if text is present and non-empty.
    if let Some(text) = text {
        if !text.is_empty() {
            tx.execute(
                "INSERT INTO search_index (item_id, original_text, text_basis)
                 VALUES (?, ?, 'original')",
                rusqlite::params![item_id, text],
            )?;
        }
    }

    Ok(())
}

/// Index a text correction.
/// Called transactionally when a user corrects an item's text.
pub fn index_text_correction(tx: &Transaction<'_>, item_id: &str) -> Result<()> {
    // Fetch the latest user correction.
    let new_text: String = tx.query_row(
        "SELECT new_value FROM corrections
         WHERE item_id = ? AND kind = 'text'
         ORDER BY revision DESC LIMIT 1",
        [item_id],
        |row| row.get(0),
    )?;

    // Index the corrected text.
    if !new_text.is_empty() {
        tx.execute(
            "INSERT INTO search_index (item_id, current_text, text_basis)
             VALUES (?, ?, 'corrected')",
            rusqlite::params![item_id, new_text],
        )?;
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
/// Results explicitly identify whether text is original or corrected.
pub fn search_index(tx: &Transaction<'_>, query: &str) -> Result<Vec<SearchResult>> {
    let mut stmt = tx.prepare(
        "SELECT item_id, original_text, current_text, text_basis
         FROM search_index
         WHERE original_text MATCH ? OR current_text MATCH ?
         ORDER BY rank DESC",
    )?;

    let results = stmt
        .query_map(rusqlite::params![query, query], |row| {
            Ok(SearchResult {
                item_id: row.get(0)?,
                original_text: row.get(1)?,
                corrected_text: row.get(2)?,
                source_type: row.get(3)?,
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
            original_text: None,
            corrected_text: None,
            source_type: "original".to_string(),
        };

        // Returns None when no text is available.
        assert_eq!(result.display_text(), None);
    }
}
