//! Sanitized artifacts, the private raw exchange file and the leak check.
//!
//! The artifact (`OHAND_LIVE_EVIDENCE_DIR`) carries no endpoint, credential, captured text or
//! model output text: only identifiers, enums, counts, sizes and timings. Raw exchanges (request
//! and response bodies, never the endpoint or credential) go to the private evidence directory
//! (`OHAND_PRIVATE_EVIDENCE_DIR`, default `~/.ohand-private-evidence`, mode 0700 with a 0600
//! file), which must be outside the repository. The artifact names that file with its run id,
//! size and SHA-256.

use super::config::{
    Backend, DEFAULT_PRIVATE_EVIDENCE_DIRECTORY, PRIVATE_EVIDENCE_DIR_ENV_VAR, RATES_ENV_VAR,
};
use super::harness::{
    build_info, instruction_version, rfc3339, summarize_by_backend, FixtureRecord, RunReport,
    ScenarioRecord,
};
use super::oracle::OracleFixture;
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("repository root")
}

/// Per-million-token rates by backend label. The file is JSON:
/// `{"anthropic:model": {"input_per_million": 3.0, "output_per_million": 15.0}}`.
pub fn load_rates() -> Option<Value> {
    let path = std::env::var(RATES_ENV_VAR).ok()?;
    let text = std::fs::read_to_string(path).expect("rates file is readable");
    Some(serde_json::from_str(&text).expect("rates file is JSON"))
}

fn cost_estimate(rates: &Option<Value>, record: &FixtureRecord) -> Value {
    let Some(rate) = rates.as_ref().map(|rates| &rates[&record.backend]) else {
        return Value::Null;
    };
    let (input, output) = record.tokens();
    match (
        rate["input_per_million"].as_f64(),
        rate["output_per_million"].as_f64(),
        input,
        output,
    ) {
        (Some(input_rate), Some(output_rate), Some(input), Some(output)) => json!({
            "label": "estimate from the supplied rates file; not a billed amount",
            "amount": (input as f64 * input_rate + output as f64 * output_rate) / 1_000_000.0,
        }),
        _ => Value::Null,
    }
}

pub struct EvidenceInputs<'a> {
    pub run_id: &'a str,
    pub collection_context: &'a str,
    pub backends: &'a [Backend],
    pub fixtures: &'a [OracleFixture],
    pub report: &'a RunReport,
    pub scenarios: &'a [ScenarioRecord],
    pub timeout_seconds: u32,
}

fn record_json(record: &FixtureRecord, rates: &Option<Value>) -> Value {
    let (input_tokens, output_tokens) = record.tokens();
    json!({
        "backend": record.backend,
        "provider": record.provider,
        "model": record.model,
        "profile_version": record.profile_version,
        "instruction_version": instruction_version(),
        "fixture_id": record.fixture_id,
        "category": record.category,
        "collected_at": record.collected_at,
        "outcome": record.run.outcome,
        "semantic_outcome": record.semantic,
        "matches_oracle": record.semantic.matches_oracle(),
        "forbidden_mutations": record.run.forbidden,
        "processing_state": record.run.state.processing_state,
        "provider_calls": record.run.exchanges.len(),
        "latency_ms_per_call": record.latencies_ms(),
        "http_status_per_call": record.run.exchanges.iter().map(|e| e.http_status).collect::<Vec<_>>(),
        "response_bytes_per_call": record.run.exchanges.iter().map(|e| e.raw_response.len()).collect::<Vec<_>>(),
        "provider_reported_tokens": {"input": input_tokens, "output": output_tokens},
        "cost_estimate": cost_estimate(rates, record),
    })
}

/// The raw exchanges, keyed by backend and fixture. Private; never committed.
pub fn raw_exchanges(report: &RunReport) -> Vec<Value> {
    report
        .records
        .iter()
        .flat_map(|record| {
            record.run.exchanges.iter().map(move |exchange| {
                json!({
                    "backend": record.backend,
                    "fixture_id": record.fixture_id,
                    "http_status": exchange.http_status,
                    "transport_failure": exchange.transport_failure,
                    "request_body": String::from_utf8_lossy(&exchange.raw_request),
                    "response_body": String::from_utf8_lossy(&exchange.raw_response),
                })
            })
        })
        .collect()
}

pub fn private_evidence_directory() -> PathBuf {
    let directory = match std::env::var(PRIVATE_EVIDENCE_DIR_ENV_VAR) {
        Ok(configured) => PathBuf::from(configured),
        Err(_) => PathBuf::from(std::env::var("HOME").expect("HOME locates the private directory"))
            .join(DEFAULT_PRIVATE_EVIDENCE_DIRECTORY),
    };
    ensure_private_directory(&directory)
}

/// Create `directory` (0700) and refuse one inside the repository.
pub fn ensure_private_directory(directory: &Path) -> PathBuf {
    std::fs::create_dir_all(directory).expect("private evidence directory");
    std::fs::set_permissions(
        directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("restrict private evidence directory");
    let resolved = directory.canonicalize().expect("resolve private directory");
    assert!(
        !resolved.starts_with(repository_root()),
        "private evidence must stay outside the repository"
    );
    resolved
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create private evidence file");
    file.write_all(bytes).expect("write private evidence file");
}

/// Fail if `text` contains any of the `forbidden` strings.
pub fn assert_no_leaks(what: &str, text: &str, forbidden: &[String]) {
    for needle in forbidden.iter().filter(|needle| needle.len() >= 4) {
        assert!(
            !text.contains(needle.as_str()),
            "{what} must not carry the endpoint, a credential, captured text or model output"
        );
    }
}

/// Strings no artifact may contain: endpoints, their hosts and origins, credentials, every
/// captured text, and `model_output_markers` supplied by the caller.
pub fn forbidden_in_artifact(
    backends: &[Backend],
    fixtures: &[OracleFixture],
    extra: &[String],
) -> Vec<String> {
    let mut forbidden = Vec::new();
    for backend in backends {
        forbidden.push(backend.credential.clone());
        if let Some(endpoint) = &backend.endpoint {
            forbidden.push(endpoint.clone());
            if let Some(origin) = super::transport::origin_of(endpoint) {
                forbidden.push(origin.trim_start_matches("https://").to_string());
            }
        }
    }
    for fixture in fixtures {
        forbidden.push(fixture.raw_input.clone());
        forbidden.extend(fixture.user_correction.clone());
    }
    forbidden.push(super::harness::PROVIDER_ONLY_TEXT.to_string());
    forbidden.extend_from_slice(extra);
    forbidden
}

/// Strings the private raw file must not contain: endpoints, origins and every credential,
/// including those of the failure scenarios. (A credential can only reach a request header,
/// never a body, so a body carrying one is a leak.)
pub fn forbidden_in_raw(backends: &[Backend]) -> Vec<String> {
    let mut forbidden = vec![
        super::harness::INVALID_SCENARIO_CREDENTIAL.to_string(),
        super::harness::UNREACHABLE_SCENARIO_CREDENTIAL.to_string(),
    ];
    for backend in backends {
        forbidden.push(backend.credential.clone());
        if let Some(endpoint) = &backend.endpoint {
            forbidden.push(endpoint.clone());
            if let Some(origin) = super::transport::origin_of(endpoint) {
                forbidden.push(origin.trim_start_matches("https://").to_string());
            }
        }
    }
    forbidden
}

pub struct Written {
    pub artifact_path: PathBuf,
    pub artifact_text: String,
    pub raw_path: PathBuf,
    pub raw_text: String,
}

/// Write the private raw file and the sanitized artifact, leak-checking both first.
pub fn write_evidence(
    inputs: &EvidenceInputs<'_>,
    artifact_dir: &Path,
    private_dir: &Path,
    extra_forbidden: &[String],
) -> Written {
    let build = build_info();
    let collected_at = rfc3339(Utc::now());
    let rates = load_rates();

    let raw_file_name = format!("{}.raw.json", inputs.run_id);
    let mut raw_text = serde_json::to_string_pretty(&json!({
        "run_id": inputs.run_id,
        "collected_at": collected_at,
        "exchanges": raw_exchanges(inputs.report),
    }))
    .unwrap();
    raw_text.push('\n');
    assert_no_leaks(
        "raw evidence",
        &raw_text,
        &forbidden_in_raw(inputs.backends),
    );
    let raw_path = private_dir.join(&raw_file_name);
    write_private_file(&raw_path, raw_text.as_bytes());
    let raw_sha256 = format!("{:x}", Sha256::digest(raw_text.as_bytes()));

    let artifact = json!({
        "schema_version": 1,
        "run_id": inputs.run_id,
        "collected_at": collected_at,
        "collection_context": inputs.collection_context,
        "build_revision": {"commit": build.commit, "dirty": build.dirty},
        "instruction_version": instruction_version(),
        "timeout_seconds": inputs.timeout_seconds,
        "backends": inputs.backends.iter().map(|b| json!({
            "label": b.label(), "protocol": b.protocol.label(), "model": b.model,
        })).collect::<Vec<_>>(),
        "fixture_count": inputs.fixtures.len(),
        "summary_by_backend": summarize_by_backend(&inputs.report.records),
        "records": inputs.report.records.iter().map(|r| record_json(r, &rates)).collect::<Vec<_>>(),
        "configure_failures": inputs.report.configure_failures.iter()
            .map(|(backend, reason)| json!({"backend": backend, "reason": reason})).collect::<Vec<_>>(),
        "capability_replaced_for_unverified_models": inputs.report.capability_overrides,
        "switching": inputs.report.switching,
        "failure_scenarios": inputs.scenarios,
        "cost": if rates.is_some() { "estimates carry a label" } else { "not computed: no rates file supplied" },
        "private_raw_evidence": {
            "run_id": inputs.run_id,
            "file_name": raw_file_name,
            "location": "private evidence directory (OHAND_PRIVATE_EVIDENCE_DIR, default ~/.ohand-private-evidence), outside Git; mode 0600 in a 0700 directory",
            "bytes": raw_text.len(),
            "sha256": raw_sha256,
            "contents": "request and response bodies of every exchange; no endpoint address or credential",
        },
    });
    let mut artifact_text = serde_json::to_string_pretty(&artifact).unwrap();
    artifact_text.push('\n');
    assert_no_leaks(
        "artifact",
        &artifact_text,
        &forbidden_in_artifact(inputs.backends, inputs.fixtures, extra_forbidden),
    );
    std::fs::create_dir_all(artifact_dir).expect("artifact directory");
    let artifact_path = artifact_dir.join(format!("{}.json", inputs.run_id));
    std::fs::write(&artifact_path, &artifact_text).expect("write artifact");
    Written {
        artifact_path,
        artifact_text,
        raw_path,
        raw_text,
    }
}
