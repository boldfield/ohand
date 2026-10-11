//! Turns the validated synthetic corpus into the frozen cases the executor runs.
//!
//! The corpus is the only input: it is loaded from the one path the caller names, parsed by the
//! evaluation tool's loader (which rejects unknown keys, duplicate ids and any fixture whose
//! provenance is not synthetic), and each fixture becomes one frozen case. Nothing is discovered,
//! and no store or capture is opened.

use evaluation::corpus::Corpus;
use ohand_core::interpretation::instructions::{content_version, M1_INSTRUCTION_TEXT};
use ohand_core::providers::contracts::{RequestedSettings, TextBasis};
use ohand_core::review::compare::FrozenComparisonSource;

use super::args::Options;
use super::profiles::derived_uuid;
use crate::executor::PlannedCase;

pub const ROUTE_ID: &str = "bench-route";
const REQUESTED_TEMPERATURE_MILLI: u32 = 0;
const REQUESTED_MAX_OUTPUT_TOKENS: u32 = 1024;

pub struct PlannedCorpus {
    pub version: String,
    pub instruction_version: String,
    pub cases: Vec<PlannedCase>,
    /// Number of fixtures in the corpus before selection.
    pub corpus_size: usize,
}

pub fn requested_settings() -> RequestedSettings {
    RequestedSettings::new()
        .with_temperature_milli(REQUESTED_TEMPERATURE_MILLI)
        .and_then(|settings| settings.with_max_output_tokens(REQUESTED_MAX_OUTPUT_TOKENS))
        .expect("fixed settings are valid")
}

pub fn load(options: &Options) -> Result<PlannedCorpus, String> {
    let corpus = Corpus::load(&options.corpus).map_err(|error| error.to_string())?;
    let instruction_version = content_version(M1_INSTRUCTION_TEXT);

    for requested in &options.case_ids {
        if corpus.fixture(requested).is_none() {
            return Err("a requested --case is not in the corpus".to_string());
        }
    }
    let mut seen_requests = options.case_ids.clone();
    seen_requests.sort();
    if seen_requests.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("--case names the same fixture twice".to_string());
    }

    let mut cases = Vec::new();
    for fixture in &corpus.fixtures {
        if !options.case_ids.is_empty() && !options.case_ids.contains(&fixture.id) {
            continue;
        }
        if options.max_cases.is_some_and(|max| cases.len() >= max) {
            break;
        }
        let context = fixture
            .case_context()
            .map_err(|error| format!("fixture {}: {error}", fixture.id))?;
        let text = context.basis_text().to_string();
        let text_basis = if context.user_correction.is_some() {
            TextBasis::Correction {
                correction_record_id: derived_uuid(&[
                    "bench-correction",
                    &corpus.version,
                    &fixture.id,
                ]),
                item_revision: 0,
            }
        } else {
            TextBasis::Original { item_revision: 0 }
        };
        let source = FrozenComparisonSource::new(
            derived_uuid(&["bench-capture", &corpus.version, &fixture.id]),
            0,
            text_basis,
            text.clone(),
            derived_uuid(&["bench-request", &corpus.version, &fixture.id]),
            context.time_context(),
            ROUTE_ID,
            M1_INSTRUCTION_TEXT,
            instruction_version.clone(),
            requested_settings(),
        )
        .map_err(|error| format!("fixture {}: {error}", fixture.id))?;
        let planned = PlannedCase::new(fixture.id.clone(), text, source)
            .map_err(|error| format!("fixture {}: {error}", fixture.id))?;
        cases.push(planned);
    }
    if cases.is_empty() {
        return Err("no fixtures were selected".to_string());
    }
    Ok(PlannedCorpus {
        version: corpus.version,
        instruction_version,
        cases,
        corpus_size: corpus.fixtures.len(),
    })
}
