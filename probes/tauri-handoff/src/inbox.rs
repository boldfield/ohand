use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::url_grammar::{parse_handoff_url, CaptureId, HandoffError};

const RECORD_EXTENSION: &str = "json";
const REJECTION_SUMMARY_FILE: &str = "rejections.json";

/// What the shell stores for one received handoff. It holds the identifier and delivery facts only; there is no
/// capture text, so nothing sensitive can reach the management web UI through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffRecord {
    pub capture_id: String,
    pub received_at_unix_ms: u64,
    /// Whether the web UI had already asked for the list when the handoff arrived. `false` shows the record was
    /// written without a running webview.
    pub webview_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RejectionSummary {
    count: u64,
    last_reason: String,
    last_received_at_unix_ms: u64,
}

/// The only data the web UI can read: identifiers in receipt order and how many handoffs were refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffSnapshot {
    pub capture_ids: Vec<String>,
    pub rejected_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    Recorded,
    /// The identifier was already recorded; the first record is kept unchanged.
    Duplicate,
}

#[derive(Debug)]
pub enum ReceiveOutcome {
    Recorded(CaptureId),
    Duplicate(CaptureId),
    Rejected(HandoffError),
    Failed(io::Error),
}

/// File-backed inbox: one file per capture ID (first write wins) plus a bounded rejection summary. Every file is
/// written to a temporary name, synced and renamed, so a crash never leaves a half-written record.
pub struct HandoffInbox {
    directory: PathBuf,
}

impl HandoffInbox {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        HandoffInbox {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn record_path(&self, capture_id: &CaptureId) -> PathBuf {
        self.directory
            .join(format!("{capture_id}.{RECORD_EXTENSION}"))
    }

    fn write_atomically(&self, destination: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let temporary = destination.with_extension("tmp");
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, destination)
    }

    pub fn record(
        &self,
        capture_id: &CaptureId,
        received_at_unix_ms: u64,
        webview_ready: bool,
    ) -> io::Result<RecordOutcome> {
        let destination = self.record_path(capture_id);
        if destination.exists() {
            return Ok(RecordOutcome::Duplicate);
        }
        let record = HandoffRecord {
            capture_id: capture_id.to_string(),
            received_at_unix_ms,
            webview_ready,
        };
        self.write_atomically(&destination, &serde_json::to_vec_pretty(&record)?)?;
        Ok(RecordOutcome::Recorded)
    }

    pub fn record_rejection(
        &self,
        reason: HandoffError,
        received_at_unix_ms: u64,
    ) -> io::Result<()> {
        let previous_count = self.rejected_count()?;
        let summary = RejectionSummary {
            count: previous_count + 1,
            last_reason: reason.code().to_owned(),
            last_received_at_unix_ms: received_at_unix_ms,
        };
        self.write_atomically(
            &self.directory.join(REJECTION_SUMMARY_FILE),
            &serde_json::to_vec_pretty(&summary)?,
        )
    }

    pub fn rejected_count(&self) -> io::Result<u64> {
        match fs::read(self.directory.join(REJECTION_SUMMARY_FILE)) {
            Ok(bytes) => {
                let summary: RejectionSummary = serde_json::from_slice(&bytes)?;
                Ok(summary.count)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Records in receipt order. Files whose name is not a canonical capture ID are ignored.
    pub fn records(&self) -> io::Result<Vec<HandoffRecord>> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let is_record_file = path.extension().and_then(|value| value.to_str())
                == Some(RECORD_EXTENSION)
                && path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|stem| stem.parse::<CaptureId>().is_ok());
            if is_record_file {
                records.push(serde_json::from_slice::<HandoffRecord>(&fs::read(&path)?)?);
            }
        }
        records.sort_by(|left, right| {
            (left.received_at_unix_ms, &left.capture_id)
                .cmp(&(right.received_at_unix_ms, &right.capture_id))
        });
        Ok(records)
    }

    pub fn snapshot(&self) -> io::Result<HandoffSnapshot> {
        Ok(HandoffSnapshot {
            capture_ids: self
                .records()?
                .into_iter()
                .map(|record| record.capture_id)
                .collect(),
            rejected_count: self.rejected_count()?,
        })
    }
}

/// Validates and stores each URL the OS delivered. This is the whole receiver: it takes no webview, window or
/// application handle, so a cold launch can record a handoff before any web UI exists. A rejected URL is counted
/// (reason only, never the raw text) and a storage failure is returned instead of being dropped.
pub fn receive_urls<'a>(
    inbox: &HandoffInbox,
    urls: impl IntoIterator<Item = &'a str>,
    webview_ready: bool,
    now_unix_ms: u64,
) -> Vec<ReceiveOutcome> {
    urls.into_iter()
        .map(|url| match parse_handoff_url(url) {
            Ok(capture_id) => match inbox.record(&capture_id, now_unix_ms, webview_ready) {
                Ok(RecordOutcome::Recorded) => ReceiveOutcome::Recorded(capture_id),
                Ok(RecordOutcome::Duplicate) => ReceiveOutcome::Duplicate(capture_id),
                Err(error) => ReceiveOutcome::Failed(error),
            },
            Err(reason) => match inbox.record_rejection(reason, now_unix_ms) {
                Ok(()) => ReceiveOutcome::Rejected(reason),
                Err(error) => ReceiveOutcome::Failed(error),
            },
        })
        .collect()
}
