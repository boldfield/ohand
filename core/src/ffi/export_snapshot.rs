//! Scoped export snapshot over the core handle (L04b).
//!
//! `ohand_core_start_export` runs the L04a export (`export::read_core_snapshot` and
//! `export::write_snapshot`) on the handle's worker thread and writes the portable text to a file
//! the native caller names. Native owns that location (a protected temporary directory) and its
//! lifetime; core only creates the file, never replaces an existing one, and removes it again when
//! the export fails part-way, so a failed call leaves no partial export behind.
//!
//! The request is one JSON object, `{"read_authentication", "scopes", "audio", "output_path"}`:
//!
//! * `read_authentication`: `authenticated` or `capture_only`, the native session read scope. Any
//!   other value is refused as `invalid_request`; `capture_only` is refused as `unauthorized`
//!   before anything is read or created.
//! * `scopes`: a non-empty list of `personal` and/or `work`. There is no default scope.
//! * `audio`: `excluded` or `references`.
//! * `output_path`: an absolute path of a file that does not exist yet.
//!
//! The outcome event for `operation_id` is `{"operation_id", "item_count", "sha256",
//! "audio_references"}`. `audio_references` lists `{item_id, capture_id, reference}` for every
//! exported item whose audio reference was included (always empty when audio is `excluded`), so
//! native can say which audio it still has to find. Core never reads audio files. Failures are
//! normalized and never echo identifiers, paths or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult, OHAND_CORE_MAX_PATH_BYTES};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::export::{
    read_core_snapshot, write_snapshot, AudioInclusion, ExportError, ExportRequest, ExportedAudio,
    ReadAuthentication,
};
use crate::providers::contracts::ErrorClass;
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    read_authentication: String,
    scopes: Vec<String>,
    audio: String,
    output_path: String,
}

const fn unauthorized() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Unauthorized,
        code: "export_not_authorized",
        message: Cow::Borrowed("export requires an authenticated read session"),
    }
}

const fn write_failed() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Permanent,
        code: "export_write_failed",
        message: Cow::Borrowed("the export could not be written"),
    }
}

struct ValidatedExport {
    request: ExportRequest,
    output_path: PathBuf,
}

impl Command {
    fn validate(&self) -> Result<ValidatedExport, AbiFailure> {
        let authentication = match self.read_authentication.as_str() {
            "authenticated" => ReadAuthentication::Authenticated,
            "capture_only" => ReadAuthentication::CaptureOnly,
            _ => return Err(AbiFailure::INVALID_REQUEST),
        };
        let audio = match self.audio.as_str() {
            "excluded" => AudioInclusion::Excluded,
            "references" => AudioInclusion::References,
            _ => return Err(AbiFailure::INVALID_REQUEST),
        };
        let scopes = self
            .scopes
            .iter()
            .map(|scope| ItemScope::from_str(scope).map_err(|_| AbiFailure::INVALID_REQUEST))
            .collect::<Result<Vec<_>, _>>()?;
        if scopes.is_empty()
            || self.output_path.is_empty()
            || self.output_path.len() > OHAND_CORE_MAX_PATH_BYTES
            || !Path::new(&self.output_path).is_absolute()
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(ValidatedExport {
            request: ExportRequest {
                authentication,
                scopes,
                audio,
            },
            output_path: PathBuf::from(&self.output_path),
        })
    }
}

fn create_exclusive(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn export_failure(error: ExportError) -> AbiFailure {
    match error {
        ExportError::NotAuthenticated => unauthorized(),
        ExportError::NoScopeSelected => AbiFailure::INVALID_REQUEST,
        ExportError::Database(error) => {
            AbiFailure::from_store(&anyhow::Error::new(error), AbiFailure::STORAGE_ERROR)
        }
        ExportError::Storage(error) => AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR),
        ExportError::Write(_) => write_failed(),
    }
}

fn run(database: &mut Database, export: &ValidatedExport) -> Result<Value, AbiFailure> {
    let snapshot =
        read_core_snapshot(database, &export.request, Utc::now()).map_err(export_failure)?;

    let file = create_exclusive(&export.output_path).map_err(|_| write_failed())?;
    let mut writer = BufWriter::new(file);
    let written = write_snapshot(&snapshot, &mut writer)
        .map_err(export_failure)
        .and_then(|summary| {
            let file = writer.into_inner().map_err(|_| write_failed())?;
            file.sync_all().map_err(|_| write_failed())?;
            Ok(summary)
        });
    let summary = match written {
        Ok(summary) => summary,
        Err(failure) => {
            let _ = std::fs::remove_file(&export.output_path);
            return Err(failure);
        }
    };

    let audio_references: Vec<Value> = snapshot
        .items
        .iter()
        .filter_map(|item| match &item.audio {
            ExportedAudio::Referenced { reference } => Some(json!({
                "item_id": item.item_id,
                "capture_id": item.capture_id,
                "reference": reference,
            })),
            ExportedAudio::None | ExportedAudio::Omitted => None,
        })
        .collect();
    Ok(json!({
        "item_count": summary.item_count,
        "sha256": summary.sha256,
        "audio_references": audio_references,
    }))
}

fn export_job(export: ValidatedExport, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &export)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one export described by the `request_len` JSON bytes at `request` (see the module
/// documentation). Unknown fields, unknown scope or audio names, an empty scope list and a
/// relative or empty `output_path` are refused as `invalid_request` before anything is queued. A
/// `capture_only` request is queued but its event is `export_not_authorized` and no file is
/// created. The outcome event for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_export(
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
        let export = command.validate()?;
        core.submit(operation_id, export_job(export, operation_id))
    })
}

#[cfg(test)]
mod tests;
