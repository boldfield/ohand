//! A key that worked and was then revoked at the provider (E02b).
//!
//! The other credential scenarios cannot show this: `invalid_hosted_credential` presents a key
//! that was never valid, and `revoked_profile_version` retires a job inside the app without any
//! provider call. This scenario has two phases, run by the maintainer with a throwaway hosted key:
//!
//! * `before`, while the key is valid: one synthetic capture runs through the production path and
//!   must apply or abstain, which shows the key worked;
//! * `after`, once the maintainer has revoked the key in the provider console: the same capture
//!   must end `unauthorized`, not retriable, after exactly one provider call, with the raw source
//!   untouched, nothing applied, no forbidden mutation, and the job failing the way it does for an
//!   invalid credential.
//!
//! ```text
//! OHAND_LIVE_PROFILES='anthropic:<model>' \
//! OHAND_LIVE_REVOCATION_CREDENTIAL_ENV='anthropic=THROWAWAY_KEY' \
//! OHAND_LIVE_REVOCATION_LABEL='<the key's name in the console>' \
//! OHAND_LIVE_REVOCATION_PHASE=before \
//! OHAND_LIVE_EVIDENCE_DIR=docs/validation/evidence/live-providers \
//!   cargo test --test live_providers live_revocation_phase -- --ignored --nocapture
//! ```
//!
//! Revoke the key, then run again with `OHAND_LIVE_REVOCATION_PHASE=after`. The label links the
//! two phases. Each phase writes a sanitized artifact (phase, label, build revision, collection
//! time, backend, outcome) that never carries the key, a fragment of it, or a hash of it. The
//! `after` phase is refused unless a passing `before` artifact with the same label and backend
//! is already in the evidence directory and is older.

use super::config::{
    parse_credential_envs, parse_profile_specs, timeout_from, Backend, PROFILES_ENV_VAR,
    TIMEOUT_ENV_VAR,
};
use super::evidence::{assert_no_leaks, assert_raw_clean, write_private_file};
use super::harness::{
    build_info, CaptureSpec, ForbiddenMutations, Harness, Outcome, PROVIDER_ONLY_TEXT,
};
use super::transport::Exchange;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CREDENTIAL_ENV_VAR: &str = "OHAND_LIVE_REVOCATION_CREDENTIAL_ENV";
pub const PHASE_ENV_VAR: &str = "OHAND_LIVE_REVOCATION_PHASE";
pub const LABEL_ENV_VAR: &str = "OHAND_LIVE_REVOCATION_LABEL";

/// Shortest slice of the credential that no artifact may contain.
const CREDENTIAL_FRAGMENT_CHARS: usize = 12;
const MAX_LABEL_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Before,
    After,
}

impl Phase {
    pub fn parse(text: &str) -> Result<Phase, String> {
        match text.trim() {
            "before" => Ok(Phase::Before),
            "after" => Ok(Phase::After),
            _ => Err(format!("{PHASE_ENV_VAR} must be before or after")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Phase::Before => "before",
            Phase::After => "after",
        }
    }
}

pub struct RevocationConfig {
    pub phase: Phase,
    pub label: String,
    pub backend: Backend,
    pub timeout_seconds: u32,
}

fn validate_label(label: &str) -> Result<(), String> {
    let allowed = |character: char| character.is_ascii_alphanumeric() || "._-".contains(character);
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS || !label.chars().all(allowed) {
        return Err(format!(
            "{LABEL_ENV_VAR} must be 1 to {MAX_LABEL_CHARS} letters, digits, '.', '_' or '-'"
        ));
    }
    Ok(())
}

impl RevocationConfig {
    /// `Ok(None)` when none of the three scenario variables is set. `lookup` reads a variable.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let (credential_spec, phase, label) = (
            lookup(CREDENTIAL_ENV_VAR),
            lookup(PHASE_ENV_VAR),
            lookup(LABEL_ENV_VAR),
        );
        let (Some(credential_spec), Some(phase), Some(label)) =
            (credential_spec.as_ref(), phase.as_ref(), label.as_ref())
        else {
            if credential_spec.is_some() || phase.is_some() || label.is_some() {
                return Err(format!(
                    "{CREDENTIAL_ENV_VAR}, {PHASE_ENV_VAR} and {LABEL_ENV_VAR} must be set together"
                ));
            }
            return Ok(None);
        };
        let phase = Phase::parse(phase)?;
        let label = label.trim().to_string();
        validate_label(&label)?;
        let named = parse_credential_envs(credential_spec)?;
        let [(protocol, variable)] =
            named
                .into_iter()
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| {
                    format!("{CREDENTIAL_ENV_VAR} must hold exactly one <provider>=<VARIABLE>")
                })?;
        if !protocol.is_hosted() {
            return Err(format!("{CREDENTIAL_ENV_VAR} must name a hosted provider"));
        }
        let model = parse_profile_specs(&lookup(PROFILES_ENV_VAR).ok_or_else(|| {
            format!("{PROFILES_ENV_VAR} must list a {} model", protocol.label())
        })?)?
        .into_iter()
        .find(|(candidate, _)| *candidate == protocol)
        .map(|(_, model)| model)
        .ok_or_else(|| format!("{PROFILES_ENV_VAR} lists no {} model", protocol.label()))?;
        let credential = lookup(&variable)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("the variable {variable} is not set"))?;
        Ok(Some(RevocationConfig {
            phase,
            label,
            backend: Backend {
                protocol,
                model,
                endpoint: None,
                credential,
            },
            timeout_seconds: timeout_from(lookup(TIMEOUT_ENV_VAR).as_deref())?,
        }))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RevocationRecord {
    pub phase: String,
    pub label: String,
    pub backend: String,
    pub outcome: Outcome,
    pub provider_calls: usize,
    pub applied_proposals: i64,
    pub capture_durable: bool,
    pub forbidden_mutations: ForbiddenMutations,
    pub job_status: Option<String>,
    pub job_failure_reason: Option<String>,
    /// The checks that did not hold; empty when `passed`.
    pub failed_checks: Vec<String>,
    pub passed: bool,
    #[serde(skip)]
    pub exchanges: Vec<Exchange>,
}

/// Run the scenario's one capture for `phase` on a fresh store with `backend`'s own credential.
pub fn run_phase(
    harness: &mut Harness,
    backend: &Backend,
    phase: Phase,
    label: &str,
) -> Result<RevocationRecord, String> {
    harness
        .configure(backend, None)
        .map_err(|reason| format!("configuration refused: {reason}"))?;
    let ingested = harness.ingest(CaptureSpec::plain(PROVIDER_ONLY_TEXT));
    let run = harness.run(&ingested, false);
    let mut failed = Vec::new();
    let mut require = |holds: bool, check: &str| {
        if !holds {
            failed.push(check.to_string());
        }
    };
    let outcome = &run.outcome;
    require(run.capture_text_durable, "capture text durable");
    require(run.forbidden.total() == 0, "no forbidden mutation");
    match phase {
        Phase::Before => {
            require(
                matches!(outcome.result.as_str(), "applied" | "abstained")
                    && outcome.route.as_deref() == Some("provider"),
                "provider call applied or abstained",
            );
            require(
                run.job_status.as_deref() == Some("completed"),
                "job completed",
            );
        }
        Phase::After => {
            require(
                outcome.result == "failed" && outcome.reason.as_deref() == Some("unauthorized"),
                "failed as unauthorized",
            );
            require(outcome.retriable == Some(false), "not retriable");
            require(run.exchanges.len() == 1, "exactly one provider call");
            require(run.state.applied_proposals == 0, "no applied proposal");
            require(
                run.state.item_type.is_none() && !run.state.has_reminder,
                "no item facet applied",
            );
            require(
                run.job_status.as_deref() == Some("failed")
                    && run
                        .job_failure_reason
                        .as_deref()
                        .is_some_and(|r| !r.is_empty()),
                "job failure visible",
            );
        }
    }
    Ok(RevocationRecord {
        phase: phase.name().to_string(),
        label: label.to_string(),
        backend: backend.label(),
        outcome: run.outcome.clone(),
        provider_calls: run.exchanges.len(),
        applied_proposals: run.state.applied_proposals,
        capture_durable: run.capture_text_durable,
        forbidden_mutations: run.forbidden.clone(),
        job_status: run.job_status.clone(),
        job_failure_reason: run.job_failure_reason.clone(),
        passed: failed.is_empty(),
        failed_checks: failed,
        exchanges: run.exchanges,
    })
}

/// The credential, its SHA-256 and every slice of it long enough to identify it.
pub fn credential_forbidden_values(credential: &str) -> Vec<String> {
    let mut forbidden = vec![credential.to_string()];
    let digest = format!("{:x}", Sha256::digest(credential.as_bytes()));
    forbidden.push(digest.to_uppercase());
    forbidden.push(digest);
    let characters: Vec<char> = credential.chars().collect();
    if characters.len() > CREDENTIAL_FRAGMENT_CHARS {
        for window in characters.windows(CREDENTIAL_FRAGMENT_CHARS) {
            forbidden.push(window.iter().collect());
        }
    }
    forbidden
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Collection time of the passing `before` artifact in `directory` for this label and backend
/// that is older than `after_collected_at`; the newest such one.
pub fn find_before_artifact(
    directory: &Path,
    label: &str,
    backend: &str,
    after_collected_at: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let mut older = Vec::new();
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(artifact) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let revocation = &artifact["revocation"];
        if revocation["phase"] != "before"
            || revocation["label"] != label
            || artifact["backend"] != backend
            || artifact["scenario"]["passed"] != true
        {
            continue;
        }
        if let Some(collected) = artifact["collected_at"].as_str().and_then(parse_time) {
            if collected < after_collected_at {
                older.push(collected);
            }
        }
    }
    older.into_iter().max().ok_or_else(|| {
        format!(
            "no passing before artifact for label {label:?} and backend {backend} older than \
             this run; run the before phase first"
        )
    })
}

pub struct RevocationWritten {
    pub artifact_path: PathBuf,
    pub artifact_text: String,
    pub raw_text: String,
}

/// Write the private raw file and the sanitized artifact, leak-checking both first. An `after`
/// artifact is refused unless the matching older `before` artifact exists in `artifact_dir`.
pub fn write_revocation_evidence(
    record: &RevocationRecord,
    backend: &Backend,
    phase: Phase,
    artifact_dir: &Path,
    private_dir: &Path,
) -> RevocationWritten {
    let build = build_info();
    let collected = Utc::now();
    let collected_at = collected.to_rfc3339_opts(SecondsFormat::Millis, true);
    if phase == Phase::After {
        if let Err(reason) =
            find_before_artifact(artifact_dir, &record.label, &record.backend, collected)
        {
            panic!("{reason}");
        }
    }
    let run_id = format!(
        "live-providers-revocation-{}-{}-{}",
        phase.name(),
        collected.format("%Y%m%dT%H%M%S%3fZ"),
        build.commit.chars().take(70).collect::<String>()
    );
    let exchanges: Vec<Value> = record
        .exchanges
        .iter()
        .map(|exchange| {
            json!({
                "backend": record.backend,
                "http_status": exchange.http_status,
                "transport_failure": exchange.transport_failure,
                "request_body": String::from_utf8_lossy(&exchange.raw_request),
                "response_body": String::from_utf8_lossy(&exchange.raw_response),
            })
        })
        .collect();
    let raw_file_name = format!("{run_id}.raw.json");
    let mut raw_text = serde_json::to_string_pretty(&json!({
        "run_id": run_id,
        "collected_at": collected_at,
        "exchanges": exchanges,
    }))
    .unwrap();
    raw_text.push('\n');
    assert_raw_clean(&exchanges, &raw_text, std::slice::from_ref(backend));
    write_private_file(&private_dir.join(&raw_file_name), raw_text.as_bytes());

    let artifact = json!({
        "schema_version": 1,
        "run_id": run_id,
        "collected_at": collected_at,
        "collection_context": format!("revocation-{}", phase.name()),
        "build_revision": {"commit": build.commit, "dirty": build.dirty},
        "revocation": {"phase": phase.name(), "label": record.label},
        "backend": record.backend,
        "outcome": record.outcome,
        "scenario": record,
        "private_raw_evidence": {
            "run_id": run_id,
            "file_name": raw_file_name,
            "location": "private evidence directory (OHAND_PRIVATE_EVIDENCE_DIR, default ~/.ohand-private-evidence), outside Git; mode 0600 in a 0700 directory",
            "bytes": raw_text.len(),
            "sha256": format!("{:x}", Sha256::digest(raw_text.as_bytes())),
            "contents": "request and response bodies of every exchange; no endpoint address or credential",
        },
    });
    let mut artifact_text = serde_json::to_string_pretty(&artifact).unwrap();
    artifact_text.push('\n');
    let mut forbidden = credential_forbidden_values(&backend.credential);
    forbidden.push(PROVIDER_ONLY_TEXT.to_string());
    assert_no_leaks("artifact", &artifact_text, &forbidden);
    std::fs::create_dir_all(artifact_dir).expect("artifact directory");
    let artifact_path = artifact_dir.join(format!("{run_id}.json"));
    std::fs::write(&artifact_path, &artifact_text).expect("write artifact");
    RevocationWritten {
        artifact_path,
        artifact_text,
        raw_text,
    }
}
