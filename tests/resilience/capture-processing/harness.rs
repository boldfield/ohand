//! Synthetic workspace, native staging stand-in and child-process control for the capture
//! durability suite. Every import goes through the real core API
//! (`ohand_core::ingress::import_foreground_ingress`); only the native staging directory, which
//! the Swift `IngressStagingStore` owns in the app, is modelled here with the same rules: stage
//! atomically via an `.incomplete` file, import, remove the record only after a confirmed import.

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
const CHILD_SCENARIO_VARIABLE: &str = "OHAND_RES_SCENARIO";
const CHILD_ROOT_VARIABLE: &str = "OHAND_RES_ROOT";
const CHILD_COUNT_VARIABLE: &str = "OHAND_RES_COUNT";
const CHILD_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn synthetic_capture_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}

pub fn synthetic_text_capture(index: usize) -> Capture {
    Capture {
        capture_id: synthetic_capture_id(index),
        text: Some(format!("synthetic capture number {index}")),
        audio_reference: None,
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

pub fn synthetic_audio_capture(index: usize) -> Capture {
    Capture {
        text: None,
        audio_reference: Some(format!("audio/clip-{index}.m4a")),
        ..synthetic_text_capture(index)
    }
}

/// A temporary application directory: one database and one staging area.
pub struct Workspace {
    _directory: TempDir,
    root: PathBuf,
}

impl Workspace {
    pub fn new() -> Workspace {
        let directory = TempDir::new().unwrap();
        let root = directory.path().to_path_buf();
        fs::create_dir_all(root.join("staging/audio")).unwrap();
        let workspace = Workspace {
            _directory: directory,
            root,
        };
        let database = workspace.open_database();
        for (route_id, scope) in [(PERSONAL_ROUTE, "personal"), (WORK_ROUTE, "work")] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-08T09:00:00Z')",
                    rusqlite::params![route_id, route_id, scope],
                )
                .unwrap();
        }
        workspace
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

    pub fn audio_path(&self, index: usize) -> PathBuf {
        self.root
            .join("staging/audio")
            .join(format!("clip-{index}.m4a"))
    }

    pub fn write_audio(&self, index: usize) -> Vec<u8> {
        let bytes = format!("synthetic audio bytes for clip {index}").into_bytes();
        fs::write(self.audio_path(index), &bytes).unwrap();
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

#[derive(Serialize, Deserialize)]
struct StagedRecord {
    capture_id: String,
    text: Option<String>,
    audio_reference: Option<String>,
    capture_instant: String,
    timezone_id: String,
    utc_offset_minutes: i32,
    locale: String,
    calendar: String,
    item_scope: String,
    route_id: String,
    entry_locked: bool,
    created_at: String,
    session_topic: Option<String>,
}

impl From<&Capture> for StagedRecord {
    fn from(capture: &Capture) -> Self {
        StagedRecord {
            capture_id: capture.capture_id.clone(),
            text: capture.text.clone(),
            audio_reference: capture.audio_reference.clone(),
            capture_instant: capture.capture_instant.clone(),
            timezone_id: capture.timezone_id.clone(),
            utc_offset_minutes: capture.utc_offset_minutes,
            locale: capture.locale.clone(),
            calendar: capture.calendar.clone(),
            item_scope: capture.item_scope.clone(),
            route_id: capture.route_id.clone(),
            entry_locked: capture.entry_locked,
            created_at: capture.created_at.clone(),
            session_topic: capture.session_topic.clone(),
        }
    }
}

impl From<StagedRecord> for Capture {
    fn from(record: StagedRecord) -> Self {
        Capture {
            capture_id: record.capture_id,
            text: record.text,
            audio_reference: record.audio_reference,
            capture_instant: record.capture_instant,
            timezone_id: record.timezone_id,
            utc_offset_minutes: record.utc_offset_minutes,
            locale: record.locale,
            calendar: record.calendar,
            item_scope: record.item_scope,
            route_id: record.route_id,
            entry_locked: record.entry_locked,
            created_at: record.created_at,
            session_topic: record.session_topic,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageOutcome {
    Created,
    AlreadyStaged,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageError {
    DifferentRecordUnderSameCaptureId,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub complete_capture_ids: Vec<String>,
    pub incomplete_file_names: Vec<String>,
}

pub struct StagingStore {
    directory: PathBuf,
}

impl StagingStore {
    pub fn at(root: &Path) -> StagingStore {
        StagingStore {
            directory: root.join("staging"),
        }
    }

    pub fn record_path(&self, capture_id: &str) -> PathBuf {
        self.directory.join(format!("{capture_id}{RECORD_SUFFIX}"))
    }

    pub fn incomplete_path(&self, capture_id: &str) -> PathBuf {
        self.directory
            .join(format!("{capture_id}{RECORD_SUFFIX}{INCOMPLETE_SUFFIX}"))
    }

    pub fn serialized(capture: &Capture) -> Vec<u8> {
        serde_json::to_vec(&StagedRecord::from(capture)).unwrap()
    }

    /// Durable and atomic: the final name appears only once every byte is synced.
    pub fn stage(&self, capture: &Capture) -> Result<StageOutcome, StageError> {
        let bytes = Self::serialized(capture);
        let final_path = self.record_path(&capture.capture_id);
        if final_path.exists() {
            return if fs::read(&final_path).unwrap() == bytes {
                Ok(StageOutcome::AlreadyStaged)
            } else {
                Err(StageError::DifferentRecordUnderSameCaptureId)
            };
        }
        let incomplete_path = self.incomplete_path(&capture.capture_id);
        let mut file = fs::File::create(&incomplete_path).unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        fs::rename(&incomplete_path, &final_path).unwrap();
        fs::File::open(&self.directory).unwrap().sync_all().unwrap();
        Ok(StageOutcome::Created)
    }

    /// Leaves a truncated `.incomplete` file and returns without renaming, as an interrupted
    /// write would.
    pub fn write_partial_only(&self, capture: &Capture) {
        let bytes = Self::serialized(capture);
        let mut file = fs::File::create(self.incomplete_path(&capture.capture_id)).unwrap();
        file.write_all(&bytes[..bytes.len() / 2]).unwrap();
        file.sync_all().unwrap();
    }

    pub fn read(&self, capture_id: &str) -> Capture {
        let bytes = fs::read(self.record_path(capture_id)).unwrap();
        serde_json::from_slice::<StagedRecord>(&bytes)
            .unwrap()
            .into()
    }

    pub fn remove(&self, capture_id: &str) {
        fs::remove_file(self.record_path(capture_id)).unwrap();
    }

    pub fn list(&self) -> Listing {
        let mut listing = Listing::default();
        for entry in fs::read_dir(&self.directory).unwrap() {
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
        let capture = staging.read(&capture_id);
        match import_foreground_ingress(&mut database, &capture) {
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
