// Save, sync, processing, reminder, and delivery states
//
// These types represent distinct facts about item status:
// - SaveState: local capture is persisted
// - SyncState: capture has been synchronized (M1: always not_configured)
// - ProcessingState: interpretation/AI processing result
// - TranscriptionState: audio transcription result
//
// Each state can be success, pending, or distinguishable errors that do not claim user attention.
// Permission denial, provider outage, expired scheduling opportunity, and pending ambiguity
// each have explicit status values.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug)]
pub struct ParseStateError(pub String);

impl fmt::Display for ParseStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseStateError {}

/// Local save state: whether the capture has been durably persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Capture has been durably saved to local storage.
    Saved,
    /// Capture is pending save (should be rare; save is fast).
    Pending,
    /// Save failed (rare; indicates storage issue).
    Failed,
}

impl SaveState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SaveState::Saved => "saved",
            SaveState::Pending => "pending",
            SaveState::Failed => "failed",
        }
    }
}

impl fmt::Display for SaveState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SaveState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "saved" => Ok(SaveState::Saved),
            "pending" => Ok(SaveState::Pending),
            "failed" => Ok(SaveState::Failed),
            _ => Err(ParseStateError(format!("Unknown SaveState: {}", s))),
        }
    }
}

/// Sync state: whether the capture has been synchronized to another device or server.
/// M1 does not support sync, so all items are "not_configured".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Sync is not configured (M1 baseline).
    NotConfigured,
    /// Sync is configured but not yet synced.
    NotSynced,
    /// Sync is pending (in progress).
    Syncing,
    /// Capture has been synced.
    Synced,
    /// Sync failed due to network/provider issue.
    SyncFailed,
}

impl SyncState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncState::NotConfigured => "not_configured",
            SyncState::NotSynced => "not_synced",
            SyncState::Syncing => "syncing",
            SyncState::Synced => "synced",
            SyncState::SyncFailed => "sync_failed",
        }
    }
}

impl fmt::Display for SyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SyncState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_configured" => Ok(SyncState::NotConfigured),
            "not_synced" => Ok(SyncState::NotSynced),
            "syncing" => Ok(SyncState::Syncing),
            "synced" => Ok(SyncState::Synced),
            "sync_failed" => Ok(SyncState::SyncFailed),
            _ => Err(ParseStateError(format!("Unknown SyncState: {}", s))),
        }
    }
}

/// Processing state: whether interpretation (AI annotation) has been performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingState {
    /// No processing has been requested or performed.
    Unprocessed,
    /// Processing is queued or in progress.
    Processing,
    /// Processing succeeded and proposal has been generated.
    Processed,
    /// Processing is pending due to authorization step.
    AwaitingAuthorization,
    /// Processing was skipped (e.g., already completed item).
    Skipped,
    /// Provider is unavailable or unreachable.
    ProviderUnavailable,
    /// User permission denied or route not configured.
    PermissionDenied,
    /// Processing failed (e.g., invalid response, timeout).
    Failed,
}

impl ProcessingState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingState::Unprocessed => "unprocessed",
            ProcessingState::Processing => "processing",
            ProcessingState::Processed => "processed",
            ProcessingState::AwaitingAuthorization => "awaiting_authorization",
            ProcessingState::Skipped => "skipped",
            ProcessingState::ProviderUnavailable => "provider_unavailable",
            ProcessingState::PermissionDenied => "permission_denied",
            ProcessingState::Failed => "failed",
        }
    }
}

impl fmt::Display for ProcessingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProcessingState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unprocessed" => Ok(ProcessingState::Unprocessed),
            "processing" => Ok(ProcessingState::Processing),
            "processed" => Ok(ProcessingState::Processed),
            "awaiting_authorization" => Ok(ProcessingState::AwaitingAuthorization),
            "skipped" => Ok(ProcessingState::Skipped),
            "provider_unavailable" => Ok(ProcessingState::ProviderUnavailable),
            "permission_denied" => Ok(ProcessingState::PermissionDenied),
            "failed" => Ok(ProcessingState::Failed),
            _ => Err(ParseStateError(format!("Unknown ProcessingState: {}", s))),
        }
    }
}

/// Transcription state: whether audio has been transcribed to text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptionState {
    /// No audio captured, or no transcription needed.
    Unprocessed,
    /// Transcription is in progress.
    Transcribing,
    /// Audio has been successfully transcribed.
    Transcribed,
    /// Transcription not attempted (no audio or not permitted).
    NotApplicable,
    /// Device/model language not supported for transcription.
    LanguageNotSupported,
    /// Transcription permissions denied.
    PermissionDenied,
    /// Transcription failed (codec, network, or service issue).
    Failed,
}

impl TranscriptionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptionState::Unprocessed => "unprocessed",
            TranscriptionState::Transcribing => "transcribing",
            TranscriptionState::Transcribed => "transcribed",
            TranscriptionState::NotApplicable => "not_applicable",
            TranscriptionState::LanguageNotSupported => "language_not_supported",
            TranscriptionState::PermissionDenied => "permission_denied",
            TranscriptionState::Failed => "failed",
        }
    }
}

impl fmt::Display for TranscriptionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TranscriptionState {
    type Err = ParseStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unprocessed" => Ok(TranscriptionState::Unprocessed),
            "transcribing" => Ok(TranscriptionState::Transcribing),
            "transcribed" => Ok(TranscriptionState::Transcribed),
            "not_applicable" => Ok(TranscriptionState::NotApplicable),
            "language_not_supported" => Ok(TranscriptionState::LanguageNotSupported),
            "permission_denied" => Ok(TranscriptionState::PermissionDenied),
            "failed" => Ok(TranscriptionState::Failed),
            _ => Err(ParseStateError(format!(
                "Unknown TranscriptionState: {}",
                s
            ))),
        }
    }
}

/// Complete status snapshot for an item.
/// Distinguishes the separate facts: save, sync, processing, transcription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemStatus {
    pub item_id: String,
    pub save_state: SaveState,
    pub sync_state: SyncState,
    pub processing_state: ProcessingState,
    pub transcription_state: TranscriptionState,
}

impl ItemStatus {
    /// Load item status from the database.
    pub fn load(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
    ) -> anyhow::Result<Option<ItemStatus>> {
        use rusqlite::OptionalExtension;

        let row: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT save_state, sync_state, processing_state, transcription_state FROM items WHERE item_id = ?",
                [item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?;

        let (save_str, sync_str, processing_str, transcription_str) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let save_state = save_str.parse::<SaveState>()?;
        let sync_state = sync_str.parse::<SyncState>()?;
        let processing_state = processing_str.parse::<ProcessingState>()?;
        let transcription_state = transcription_str.parse::<TranscriptionState>()?;

        Ok(Some(ItemStatus {
            item_id: item_id.to_string(),
            save_state,
            sync_state,
            processing_state,
            transcription_state,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_state_roundtrip() {
        for state in [SaveState::Saved, SaveState::Pending, SaveState::Failed] {
            let s = state.as_str();
            let parsed: SaveState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_sync_state_roundtrip() {
        for state in [
            SyncState::NotConfigured,
            SyncState::NotSynced,
            SyncState::Syncing,
            SyncState::Synced,
            SyncState::SyncFailed,
        ] {
            let s = state.as_str();
            let parsed: SyncState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_sync_state_m1_baseline() {
        assert_eq!(SyncState::NotConfigured.as_str(), "not_configured");
        let parsed: SyncState = "not_configured".parse().expect("should parse");
        assert_eq!(parsed, SyncState::NotConfigured);
    }

    #[test]
    fn test_processing_state_roundtrip() {
        for state in [
            ProcessingState::Unprocessed,
            ProcessingState::Processing,
            ProcessingState::Processed,
            ProcessingState::AwaitingAuthorization,
            ProcessingState::Skipped,
            ProcessingState::ProviderUnavailable,
            ProcessingState::PermissionDenied,
            ProcessingState::Failed,
        ] {
            let s = state.as_str();
            let parsed: ProcessingState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_processing_state_distinguishable_errors() {
        let permission = ProcessingState::PermissionDenied;
        let outage = ProcessingState::ProviderUnavailable;
        let ambiguity = ProcessingState::AwaitingAuthorization;

        assert_eq!(permission.as_str(), "permission_denied");
        assert_eq!(outage.as_str(), "provider_unavailable");
        assert_eq!(ambiguity.as_str(), "awaiting_authorization");

        assert_ne!(permission, outage);
        assert_ne!(outage, ambiguity);
        assert_ne!(permission, ambiguity);
    }

    #[test]
    fn test_transcription_state_roundtrip() {
        for state in [
            TranscriptionState::Unprocessed,
            TranscriptionState::Transcribing,
            TranscriptionState::Transcribed,
            TranscriptionState::NotApplicable,
            TranscriptionState::LanguageNotSupported,
            TranscriptionState::PermissionDenied,
            TranscriptionState::Failed,
        ] {
            let s = state.as_str();
            let parsed: TranscriptionState = s.parse().expect("should parse");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_save_state_not_claiming_attention() {
        let saved = SaveState::Saved;
        let failed = SaveState::Failed;
        let pending = SaveState::Pending;

        assert_eq!(saved.as_str(), "saved");
        assert_eq!(failed.as_str(), "failed");
        assert_eq!(pending.as_str(), "pending");
        // States are just values; UI/orchestration decides what claims attention.
    }

    #[test]
    fn test_invalid_state_strings() {
        assert!("invalid".parse::<SaveState>().is_err());
        assert!("invalid".parse::<SyncState>().is_err());
        assert!("invalid".parse::<ProcessingState>().is_err());
        assert!("invalid".parse::<TranscriptionState>().is_err());
    }
}
