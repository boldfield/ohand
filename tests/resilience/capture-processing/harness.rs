//! Synthetic workspace, native staging model and child-process control for the capture
//! durability suite. Every import goes through the real core API
//! (`ohand_core::ingress::import_foreground_ingress`) against a real SQLite file.
//!
//! The native side is a Rust model, not the Swift code. `StagingStore` and `drain_staging_at`
//! reproduce the durability rules of `ios/Services/Ingress/IngressStagingStore.swift` and
//! `ForegroundIngressService.process`: a record is written through an `.incomplete` file and an
//! atomic rename; audio moves (never copies) from the in-progress store to the finalized store
//! before import, idempotently, and a destination plus source together is a conflict that
//! touches neither; a record is removed only after a confirmed import or an `ItemDeleted`
//! outcome; everything else is kept. Differences: the JSON field names are this model's own,
//! there are no file-protection or backup attributes, no unreadable-record or concurrent-waiter
//! handling, and no deletion-cleanup path. The Swift service itself is covered against the real
//! core by the macOS simulator suite, not by this harness.

use ohand_core::ingress::{
    import_foreground_ingress, IngressError, IngressErrorKind, IngressImportAcknowledgment,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Database, SystemClock};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

pub const PERSONAL_ROUTE: &str = "route-personal";
pub const WORK_ROUTE: &str = "route-work";
pub const PROTOCOL_PREFIX: &str = "OHAND-RES ";
const RECORD_SUFFIX: &str = ".json";
const INCOMPLETE_SUFFIX: &str = ".incomplete";
const RECORDS_DIRECTORY: &str = "IngressRecords";
const IN_PROGRESS_AUDIO_DIRECTORY: &str = "IngressAudio";
const FINALIZED_AUDIO_DIRECTORY: &str = "FinalizedAudio";
const CURRENT_FORMAT_VERSION: u32 = 1;
const CHILD_SCENARIO_VARIABLE: &str = "OHAND_RES_SCENARIO";
const CHILD_ROOT_VARIABLE: &str = "OHAND_RES_ROOT";
const CHILD_COUNT_VARIABLE: &str = "OHAND_RES_COUNT";
const CHILD_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn synthetic_capture_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureContext {
    pub capture_instant: String,
    pub timezone_id: String,
    pub utc_offset_minutes: i32,
    pub locale: String,
    pub calendar: String,
    pub item_scope: String,
    pub route_id: String,
    pub entry_locked: bool,
    pub created_at: String,
    pub session_topic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioHandoff {
    pub in_progress_file_name: String,
    pub finalized_file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedRecord {
    pub format_version: u32,
    pub capture_id: String,
    pub text: Option<String>,
    pub audio: Option<AudioHandoff>,
    pub context: CaptureContext,
}

fn synthetic_context() -> CaptureContext {
    CaptureContext {
        capture_instant: "2026-10-08T09:30:00Z".to_string(),
        timezone_id: "America/New_York".to_string(),
        utc_offset_minutes: -240,
        locale: "en_US".to_string(),
        calendar: "gregorian".to_string(),
        item_scope: "personal".to_string(),
        route_id: PERSONAL_ROUTE.to_string(),
        entry_locked: false,
        created_at: "2026-10-08T09:30:01Z".to_string(),
        session_topic: None,
    }
}

pub fn synthetic_text_record(index: usize) -> StagedRecord {
    StagedRecord {
        format_version: CURRENT_FORMAT_VERSION,
        capture_id: synthetic_capture_id(index),
        text: Some(format!("synthetic capture number {index}")),
        audio: None,
        context: synthetic_context(),
    }
}

pub fn synthetic_audio_record(index: usize) -> StagedRecord {
    let capture_id = synthetic_capture_id(index);
    StagedRecord {
        format_version: CURRENT_FORMAT_VERSION,
        text: None,
        audio: Some(AudioHandoff {
            in_progress_file_name: format!("recording-{index}.m4a"),
            finalized_file_name: format!("{capture_id}.m4a"),
        }),
        capture_id,
        context: synthetic_context(),
    }
}

pub fn synthetic_audio_bytes(index: usize) -> Vec<u8> {
    format!("synthetic audio bytes for clip {index}").into_bytes()
}

impl StagedRecord {
    /// The reference the core keeps for an audio capture: inside the finalized audio store.
    pub fn core_audio_reference(&self) -> Option<String> {
        self.audio
            .as_ref()
            .map(|audio| format!("{FINALIZED_AUDIO_DIRECTORY}/{}", audio.finalized_file_name))
    }

    pub fn core_capture(&self) -> Capture {
        Capture {
            capture_id: self.capture_id.clone(),
            text: self.text.clone(),
            audio_reference: self.core_audio_reference(),
            capture_instant: self.context.capture_instant.clone(),
            timezone_id: self.context.timezone_id.clone(),
            utc_offset_minutes: self.context.utc_offset_minutes,
            locale: self.context.locale.clone(),
            calendar: self.context.calendar.clone(),
            item_scope: self.context.item_scope.clone(),
            route_id: self.context.route_id.clone(),
            entry_locked: self.context.entry_locked,
            created_at: self.context.created_at.clone(),
            session_topic: self.context.session_topic.clone(),
        }
    }

    fn validate(&self) -> Result<(), StageError> {
        let identifier_character = |character: char| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        };
        let safe_capture_id = !self.capture_id.is_empty()
            && self.capture_id.len() <= 128
            && self.capture_id.chars().all(identifier_character);
        let safe_file_name = |name: &str| {
            !name.is_empty()
                && name.len() <= 200
                && !name.starts_with('.')
                && !name.contains("..")
                && name
                    .chars()
                    .all(|character| identifier_character(character) || character == '.')
        };
        if !safe_capture_id {
            return Err(StageError::Invalid("unsafe capture id"));
        }
        match (&self.text, &self.audio) {
            (Some(text), None) if !text.is_empty() => Ok(()),
            (Some(_), None) => Err(StageError::Invalid("empty text")),
            (None, Some(audio)) => {
                if !safe_file_name(&audio.in_progress_file_name)
                    || !safe_file_name(&audio.finalized_file_name)
                {
                    Err(StageError::Invalid("unsafe file name"))
                } else if !audio
                    .finalized_file_name
                    .starts_with(&format!("{}.", self.capture_id))
                {
                    Err(StageError::Invalid("finalized name not bound to capture"))
                } else {
                    Ok(())
                }
            }
            _ => Err(StageError::Invalid("exactly one source required")),
        }
    }
}

/// A temporary application directory: one database, the record store and the two audio stores.
pub struct Workspace {
    _directory: TempDir,
    root: PathBuf,
}

impl Workspace {
    pub fn new() -> Workspace {
        let directory = TempDir::new().unwrap();
        let root = directory.path().to_path_buf();
        for name in [
            RECORDS_DIRECTORY,
            IN_PROGRESS_AUDIO_DIRECTORY,
            FINALIZED_AUDIO_DIRECTORY,
        ] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let workspace = Workspace {
            _directory: directory,
            root,
        };
        let database = workspace.open_database();
        for (route_id, scope) in [(PERSONAL_ROUTE, "personal"), (WORK_ROUTE, "work")] {
            workspace.insert_route(&database, route_id, scope);
        }
        workspace
    }

    pub fn insert_route(&self, database: &Database, route_id: &str, scope: &str) {
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                 VALUES (?, ?, ?, '[]', '2026-10-08T09:00:00Z')",
                rusqlite::params![route_id, route_id, scope],
            )
            .unwrap();
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opening again stands in for a process restart.
    pub fn open_database(&self) -> Database {
        open_database_at(&self.root)
    }

    pub fn staging(&self) -> StagingStore {
        StagingStore::at(&self.root)
    }

    pub fn in_progress_audio_path(&self, file_name: &str) -> PathBuf {
        self.root.join(IN_PROGRESS_AUDIO_DIRECTORY).join(file_name)
    }

    pub fn finalized_audio_path(&self, file_name: &str) -> PathBuf {
        self.root.join(FINALIZED_AUDIO_DIRECTORY).join(file_name)
    }

    /// Writes the synthetic recording for `record` into the in-progress store and returns its bytes.
    pub fn write_in_progress_audio(&self, record: &StagedRecord, index: usize) -> Vec<u8> {
        let bytes = synthetic_audio_bytes(index);
        let audio = record.audio.as_ref().unwrap();
        fs::write(
            self.in_progress_audio_path(&audio.in_progress_file_name),
            &bytes,
        )
        .unwrap();
        bytes
    }

    /// Imports every complete staged record and removes it only on a confirmed import (or the
    /// one terminal outcome, a deleted item). Anything else, and every incomplete write, stays.
    pub fn drain_staging(&self) -> DrainReport {
        drain_staging_at(&self.root)
    }

    pub fn count(&self, table: &str) -> i64 {
        count_rows(&self.open_database(), table)
    }

    pub fn stored_audio_reference(&self, capture_id: &str) -> Option<String> {
        self.open_database()
            .conn()
            .query_row(
                "SELECT audio_reference FROM captures WHERE capture_id = ?",
                [capture_id],
                |row| row.get(0),
            )
            .unwrap()
    }
}

pub fn open_database_at(root: &Path) -> Database {
    let path = root.join("ohand.db").display().to_string();
    Database::open(&path, Arc::new(SystemClock)).unwrap()
}

pub fn count_rows(database: &Database, table: &str) -> i64 {
    database
        .conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

pub fn item_ids_for(database: &Database, capture_id: &str) -> Vec<String> {
    let mut statement = database
        .conn()
        .prepare("SELECT item_id FROM items WHERE capture_id = ? ORDER BY item_id")
        .unwrap();
    let rows = statement
        .query_map([capture_id], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

pub fn stored_capture_ids(database: &Database) -> Vec<String> {
    let mut statement = database
        .conn()
        .prepare("SELECT capture_id FROM captures ORDER BY capture_id")
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageOutcome {
    Created,
    AlreadyStaged,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageError {
    DifferentRecordUnderSameCaptureId,
    Invalid(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum AudioFinalization {
    Ready { moved: bool },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AudioFailure {
    DestinationConflict,
    SourceMissing,
    SourceEmpty,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadFailure {
    Corrupt,
    UnsupportedVersion(u32),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub complete_capture_ids: Vec<String>,
    pub incomplete_file_names: Vec<String>,
}

pub struct StagingStore {
    root: PathBuf,
}

impl StagingStore {
    pub fn at(root: &Path) -> StagingStore {
        StagingStore {
            root: root.to_path_buf(),
        }
    }

    fn records_directory(&self) -> PathBuf {
        self.root.join(RECORDS_DIRECTORY)
    }

    pub fn record_path(&self, capture_id: &str) -> PathBuf {
        self.records_directory()
            .join(format!("{capture_id}{RECORD_SUFFIX}"))
    }

    pub fn incomplete_path(&self, capture_id: &str) -> PathBuf {
        self.records_directory()
            .join(format!("{capture_id}{RECORD_SUFFIX}{INCOMPLETE_SUFFIX}"))
    }

    pub fn serialized(record: &StagedRecord) -> Vec<u8> {
        serde_json::to_vec(record).unwrap()
    }

    /// Durable and atomic: the final name appears only once every byte is synced. An existing
    /// record with the same capture ID is left alone: identical content is a repeated delivery,
    /// different content is a conflict.
    pub fn stage(&self, record: &StagedRecord) -> Result<StageOutcome, StageError> {
        record.validate()?;
        let final_path = self.record_path(&record.capture_id);
        if final_path.exists() {
            let existing: StagedRecord = serde_json::from_slice(&fs::read(&final_path).unwrap())
                .expect("a record this model staged is readable");
            return if &existing == record {
                Ok(StageOutcome::AlreadyStaged)
            } else {
                Err(StageError::DifferentRecordUnderSameCaptureId)
            };
        }
        let incomplete_path = self.incomplete_path(&record.capture_id);
        let mut file = fs::File::create(&incomplete_path).unwrap();
        file.write_all(&Self::serialized(record)).unwrap();
        file.sync_all().unwrap();
        fs::rename(&incomplete_path, &final_path).unwrap();
        fs::File::open(self.records_directory())
            .unwrap()
            .sync_all()
            .unwrap();
        Ok(StageOutcome::Created)
    }

    /// Leaves a truncated `.incomplete` file and returns without renaming, as an interrupted
    /// write would.
    pub fn write_partial_only(&self, record: &StagedRecord) {
        let bytes = Self::serialized(record);
        let mut file = fs::File::create(self.incomplete_path(&record.capture_id)).unwrap();
        file.write_all(&bytes[..bytes.len() / 2]).unwrap();
        file.sync_all().unwrap();
    }

    pub fn load(&self, capture_id: &str) -> Result<StagedRecord, LoadFailure> {
        let bytes = fs::read(self.record_path(capture_id)).unwrap();
        let record: StagedRecord =
            serde_json::from_slice(&bytes).map_err(|_| LoadFailure::Corrupt)?;
        if record.format_version > CURRENT_FORMAT_VERSION {
            return Err(LoadFailure::UnsupportedVersion(record.format_version));
        }
        if record.capture_id != capture_id {
            return Err(LoadFailure::Corrupt);
        }
        Ok(record)
    }

    pub fn remove(&self, capture_id: &str) {
        let path = self.record_path(capture_id);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }

    pub fn list(&self) -> Listing {
        let mut listing = Listing::default();
        for entry in fs::read_dir(self.records_directory()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.ends_with(INCOMPLETE_SUFFIX) {
                listing.incomplete_file_names.push(name);
            } else if let Some(capture_id) = name.strip_suffix(RECORD_SUFFIX) {
                listing.complete_capture_ids.push(capture_id.to_string());
            }
        }
        listing.complete_capture_ids.sort();
        listing.incomplete_file_names.sort();
        listing
    }

    /// Moves the recording to the finalized store. Idempotent: a finalized file with the source
    /// gone means an earlier attempt of this same record completed the move. Both files present
    /// is a conflict that touches neither; so is an empty file or a missing source.
    pub fn finalize_audio(&self, audio: &AudioHandoff) -> Result<AudioFinalization, AudioFailure> {
        let source = self
            .root
            .join(IN_PROGRESS_AUDIO_DIRECTORY)
            .join(&audio.in_progress_file_name);
        let destination = self
            .root
            .join(FINALIZED_AUDIO_DIRECTORY)
            .join(&audio.finalized_file_name);
        let require_non_empty = |path: &Path| {
            if fs::metadata(path).unwrap().len() == 0 {
                Err(AudioFailure::SourceEmpty)
            } else {
                Ok(())
            }
        };
        if destination.exists() {
            if source.exists() {
                return Err(AudioFailure::DestinationConflict);
            }
            require_non_empty(&destination)?;
            return Ok(AudioFinalization::Ready { moved: false });
        }
        if !source.exists() {
            return Err(AudioFailure::SourceMissing);
        }
        require_non_empty(&source)?;
        fs::rename(&source, &destination).unwrap();
        fs::File::open(self.root.join(FINALIZED_AUDIO_DIRECTORY))
            .unwrap()
            .sync_all()
            .unwrap();
        Ok(AudioFinalization::Ready { moved: true })
    }
}

#[derive(Debug, Default)]
pub struct DrainReport {
    pub acknowledgments: Vec<IngressImportAcknowledgment>,
    pub kept_after_failure: Vec<(String, String)>,
}

pub fn drain_staging_at(root: &Path) -> DrainReport {
    let staging = StagingStore::at(root);
    let mut database = open_database_at(root);
    let mut report = DrainReport::default();
    for capture_id in staging.list().complete_capture_ids {
        let record = match staging.load(&capture_id) {
            Ok(record) => record,
            Err(failure) => {
                report
                    .kept_after_failure
                    .push((capture_id, format!("{failure:?}")));
                continue;
            }
        };
        if let Some(audio) = &record.audio {
            if let Err(failure) = staging.finalize_audio(audio) {
                report
                    .kept_after_failure
                    .push((capture_id, format!("{failure:?}")));
                continue;
            }
        }
        match import_foreground_ingress(&mut database, &record.core_capture()) {
            Ok(acknowledgment) => {
                staging.remove(&capture_id);
                report.acknowledgments.push(acknowledgment);
            }
            Err(IngressError {
                kind: IngressErrorKind::ItemDeleted { .. },
                ..
            }) => staging.remove(&capture_id),
            Err(error) => report
                .kept_after_failure
                .push((capture_id, error.to_string())),
        }
    }
    report
}

/// A re-executed copy of this test binary running one named scenario, so a hard kill takes out
/// a real process that holds a real database connection.
pub struct ChildProcess {
    child: Child,
    messages: Receiver<String>,
}

impl ChildProcess {
    pub fn spawn(scenario: &str, root: &Path, count: usize) -> ChildProcess {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child::child_entry",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_SCENARIO_VARIABLE, scenario)
            .env(CHILD_ROOT_VARIABLE, root)
            .env(CHILD_COUNT_VARIABLE, count.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(message) = line.strip_prefix(PROTOCOL_PREFIX) {
                    if sender.send(message.to_string()).is_err() {
                        break;
                    }
                }
            }
        });
        ChildProcess { child, messages }
    }

    /// The next protocol message; fails the test if the child goes quiet or exits first.
    pub fn next_message(&self) -> String {
        self.messages
            .recv_timeout(CHILD_MESSAGE_TIMEOUT)
            .expect("child produced no further protocol message")
    }

    /// The next protocol message if one arrives within `wait`.
    pub fn poll_message(&self, wait: Duration) -> Option<String> {
        self.messages.recv_timeout(wait).ok()
    }

    pub fn expect_message_starting_with(&self, prefix: &str) -> String {
        let message = self.next_message();
        assert!(
            message.starts_with(prefix),
            "expected a '{prefix}' message, got '{message}'"
        );
        message
    }

    /// SIGKILL: no destructors, no flush, no transaction rollback by the child.
    pub fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

pub struct ChildScenario {
    pub name: String,
    pub root: PathBuf,
    pub count: usize,
}

/// `Some` only inside a spawned child.
pub fn child_scenario_from_environment() -> Option<ChildScenario> {
    let name = std::env::var(CHILD_SCENARIO_VARIABLE).ok()?;
    Some(ChildScenario {
        name,
        root: PathBuf::from(std::env::var(CHILD_ROOT_VARIABLE).unwrap()),
        count: std::env::var(CHILD_COUNT_VARIABLE)
            .unwrap()
            .parse()
            .unwrap(),
    })
}
