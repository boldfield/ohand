//! The synthetic intent corpus (`fixtures/intent/contrastive-fixtures.json`) as evaluation input.
//!
//! The types mirror `docs/features/fixture-schema.md` and reject unknown keys, so a forbidden
//! rule this tool does not understand fails the load instead of going unenforced.

use chrono::{DateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use ohand_core::interpretation::contracts::{AbstentionReason, TimeResolutionQuality};
use ohand_core::store::events::ItemType;
use ohand_core::time::TimeContext;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::EvaluationError;

pub const DEFAULT_CAPTURE_INSTANT: &str = "2026-10-08T14:00:00Z";
pub const DEFAULT_TIMEZONE: &str = "America/New_York";
pub const EVALUATION_LOCALE: &str = "en-US";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusFile {
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub id: String,
    pub category: Category,
    pub input: String,
    pub provenance: String,
    pub text_basis: BasisKind,
    #[allow(dead_code)]
    preserve: Preserve,
    pub capture_context: CaptureContext,
    pub expected: Expected,
    pub forbidden: Forbidden,
    pub recoverable: bool,
    #[allow(dead_code)]
    recovery_notes: Option<String>,
    #[allow(dead_code)]
    notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Design,
    Dates,
    Mixed,
    Corrections,
    Ambiguity,
    BroadIntention,
    Negation,
    PromptInjection,
    DroppedAsrWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BasisKind {
    Original,
    Corrected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preserve {
    #[allow(dead_code)]
    raw_input: bool,
    #[allow(dead_code)]
    user_correction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CaptureType {
    Text,
    Voice,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureContext {
    #[allow(dead_code)]
    capture_type: CaptureType,
    pub capture_instant: Option<String>,
    pub device_timezone: Option<String>,
    pub user_correction: Option<String>,
    #[allow(dead_code)]
    correction_spans: Option<Vec<CorrectionSpan>>,
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectionSpan {
    #[allow(dead_code)]
    original: String,
    #[allow(dead_code)]
    corrected: String,
}

/// The reference outcome. A facet absent here must also be absent from the evaluated outcome.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub item_type: Option<ItemType>,
    pub source_spans: Option<Vec<OracleSpan>>,
    pub reminder_proposal: Option<OracleReminder>,
    pub session_topic_proposal: Option<OracleTopic>,
    pub abstention: Option<AbstentionReason>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleSpan {
    pub start: usize,
    pub end: usize,
    #[allow(dead_code)]
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleReminder {
    pub instant: Option<String>,
    pub timezone_id: Option<String>,
    pub quality: TimeResolutionQuality,
    pub source_span: OracleSpan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleTopic {
    pub topic: String,
    pub source_span: OracleSpan,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Forbidden {
    pub item_types: Vec<ItemType>,
    pub reminder: Option<ReminderBan>,
    pub reminder_qualities: Vec<TimeResolutionQuality>,
    pub reminder_timezones: Vec<String>,
    pub session_topic: bool,
    pub facets: Option<FacetBan>,
    pub operations: Vec<ForbiddenOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderBan {
    Any,
    AnyInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetBan {
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenOperation {
    Update,
    Create,
}

/// Everything a pipeline needs to treat one fixture as a capture: the preserved raw input, the
/// optional user correction, the text spans index into and the time context.
#[derive(Debug, Clone)]
pub struct CaseContext {
    pub raw_input: String,
    pub user_correction: Option<String>,
    pub capture_instant: DateTime<Utc>,
    pub timezone: String,
    pub utc_offset_seconds: i32,
}

impl CaseContext {
    /// The effective text of the capture: the correction when one exists, else the raw input.
    pub fn basis_text(&self) -> &str {
        self.user_correction.as_deref().unwrap_or(&self.raw_input)
    }

    pub fn time_context(&self) -> TimeContext {
        TimeContext {
            timezone: self.timezone.clone(),
            locale: EVALUATION_LOCALE.to_string(),
            reference_time: self.capture_instant,
            utc_offset_at_capture: self.utc_offset_seconds,
            calendar: "gregorian".to_string(),
        }
    }
}

impl Fixture {
    pub fn case_context(&self) -> Result<CaseContext, EvaluationError> {
        let instant_text = self
            .capture_context
            .capture_instant
            .as_deref()
            .unwrap_or(DEFAULT_CAPTURE_INSTANT);
        let capture_instant = DateTime::parse_from_rfc3339(instant_text)
            .map_err(|error| {
                EvaluationError::Corpus(format!(
                    "fixture {}: capture_instant {instant_text:?}: {error}",
                    self.id
                ))
            })?
            .with_timezone(&Utc);
        let timezone = self
            .capture_context
            .device_timezone
            .clone()
            .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
        let zone: Tz = timezone.parse().map_err(|_| {
            EvaluationError::Corpus(format!(
                "fixture {}: {timezone:?} is not an IANA zone",
                self.id
            ))
        })?;
        let utc_offset_seconds = zone
            .offset_from_utc_datetime(&capture_instant.naive_utc())
            .fix()
            .local_minus_utc();
        let user_correction = match self.text_basis {
            BasisKind::Original => None,
            BasisKind::Corrected => Some(self.capture_context.user_correction.clone().ok_or_else(
                || {
                    EvaluationError::Corpus(format!(
                        "fixture {}: corrected basis without a user_correction",
                        self.id
                    ))
                },
            )?),
        };
        Ok(CaseContext {
            raw_input: self.input.clone(),
            user_correction,
            capture_instant,
            timezone,
            utc_offset_seconds,
        })
    }
}

/// A loaded corpus and the content digest that identifies its version in every report.
#[derive(Debug)]
pub struct Corpus {
    pub version: String,
    pub fixtures: Vec<Fixture>,
}

impl Corpus {
    pub fn from_bytes(bytes: &[u8]) -> Result<Corpus, EvaluationError> {
        let file: CorpusFile = serde_json::from_slice(bytes)
            .map_err(|error| EvaluationError::Corpus(error.to_string()))?;
        let mut seen = std::collections::BTreeSet::new();
        for fixture in &file.fixtures {
            if !seen.insert(fixture.id.as_str()) {
                return Err(EvaluationError::Corpus(format!(
                    "duplicate fixture id {}",
                    fixture.id
                )));
            }
            if !fixture.provenance.starts_with("synthetic") {
                return Err(EvaluationError::Corpus(format!(
                    "fixture {} is not synthetic: provenance {:?}",
                    fixture.id, fixture.provenance
                )));
            }
        }
        if file.fixtures.is_empty() {
            return Err(EvaluationError::Corpus("corpus has no fixtures".into()));
        }
        Ok(Corpus {
            version: content_version(bytes),
            fixtures: file.fixtures,
        })
    }

    pub fn load(path: &Path) -> Result<Corpus, EvaluationError> {
        let bytes = std::fs::read(path)
            .map_err(|error| EvaluationError::Io(format!("{}: {error}", path.display())))?;
        Corpus::from_bytes(&bytes)
    }

    pub fn fixture(&self, id: &str) -> Option<&Fixture> {
        self.fixtures.iter().find(|fixture| fixture.id == id)
    }
}

/// `sha256:<hex>` of the exact file bytes.
pub fn content_version(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::from("sha256:");
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}
