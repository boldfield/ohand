//! Recorded provider-response fixtures (`tools/evaluation/fixtures/recorded-responses.json`).
//!
//! Each scenario is one provider reply to one corpus fixture. Unless a scenario carries a
//! `live_run` provenance with the full authorization record, it is a synthetic authored scenario
//! and is executed and reported as `fake`; only a complete `live_run` provenance makes it
//! `recorded`. Provenance is validated on load, so a half-attached claim fails instead of being
//! quietly downgraded or upgraded.

use chrono::DateTime;
use ohand_core::providers::contracts::TransportError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

use crate::corpus::{content_version, Corpus};
use crate::EvaluationError;

pub const RESPONSES_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesFile {
    format_version: u32,
    responses: Vec<Scenario>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub scenario_id: String,
    pub fixture_id: String,
    pub role: Role,
    pub provenance: Provenance,
    pub description: String,
    pub behavior: Behavior,
}

/// Whether the scripted reply is meant to follow the output contract or to misbehave. Adversarial
/// scenarios document what the application guard does with an untrusted reply; neither role says
/// anything about how a real model behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Conforming,
    Adversarial,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance {
    /// Authored for this repository. Proves nothing about any provider.
    SyntheticAuthored,
    /// Captured from a specific authorized live run against synthetic corpus inputs.
    LiveRun {
        run_id: String,
        authorized_by: String,
        collected_at: String,
        provider_profile: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Behavior {
    /// The transport returns this JSON object as the reply body.
    Respond { body: serde_json::Value },
    /// The transport returns these exact bytes, valid JSON or not.
    RespondRaw { body: String },
    /// The transport fails before any reply.
    TransportError { error: TransportError },
}

#[derive(Debug)]
pub struct Responses {
    pub version: String,
    pub scenarios: Vec<Scenario>,
}

impl Responses {
    pub fn from_bytes(bytes: &[u8], corpus: &Corpus) -> Result<Responses, EvaluationError> {
        let file: ResponsesFile = serde_json::from_slice(bytes)
            .map_err(|error| EvaluationError::Responses(error.to_string()))?;
        if file.format_version != RESPONSES_FORMAT_VERSION {
            return Err(EvaluationError::Responses(format!(
                "format_version {} is not supported (expected {RESPONSES_FORMAT_VERSION})",
                file.format_version
            )));
        }
        let mut seen = BTreeSet::new();
        for scenario in &file.responses {
            let fail = |reason: String| {
                Err(EvaluationError::Responses(format!(
                    "scenario {}: {reason}",
                    scenario.scenario_id
                )))
            };
            if scenario.scenario_id.trim().is_empty() || !seen.insert(scenario.scenario_id.clone())
            {
                return fail("scenario_id must be unique and non-empty".into());
            }
            if corpus.fixture(&scenario.fixture_id).is_none() {
                return fail(format!(
                    "fixture_id {:?} is not in the corpus",
                    scenario.fixture_id
                ));
            }
            if scenario.description.trim().is_empty() {
                return fail("description must say what the scenario demonstrates".into());
            }
            if let Provenance::LiveRun {
                run_id,
                authorized_by,
                collected_at,
                provider_profile,
            } = &scenario.provenance
            {
                for (name, value) in [
                    ("run_id", run_id),
                    ("authorized_by", authorized_by),
                    ("provider_profile", provider_profile),
                ] {
                    if value.trim().is_empty() {
                        return fail(format!("live_run provenance needs a non-empty {name}"));
                    }
                }
                if DateTime::parse_from_rfc3339(collected_at).is_err() {
                    return fail("live_run collected_at must be an RFC 3339 instant".into());
                }
            }
            if let Behavior::Respond { body } = &scenario.behavior {
                if !body.is_object() {
                    return fail("a respond body must be a JSON object".into());
                }
            }
        }
        Ok(Responses {
            version: content_version(bytes),
            scenarios: file.responses,
        })
    }

    pub fn load(path: &Path, corpus: &Corpus) -> Result<Responses, EvaluationError> {
        let bytes = std::fs::read(path)
            .map_err(|error| EvaluationError::Io(format!("{}: {error}", path.display())))?;
        Responses::from_bytes(&bytes, corpus)
    }
}
