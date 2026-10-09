//! Live smoke run of `SelfHostedAdapter` against the configured Spark endpoint (V09).
//!
//! Ignored by default: it needs the worker-mounted provider configuration and network access.
//! Run it with
//! `OHAND_SMOKE_EVIDENCE_DIR=docs/validation/evidence/spark-adapter-smoke \
//!  cargo test --test providers_self_hosted live_endpoint_smoke -- --ignored --nocapture`.
//!
//! The adapter itself builds and decodes every request, exactly as in production. Only the
//! transport differs: a `curl` subprocess stands in for the native HTTPS transport (https only,
//! certificate validation on, redirects not followed). The credential is passed to curl on
//! stdin, never on the command line. The written artifact carries no endpoint, credential,
//! captured text or model output text: only status, finish reason, sizes and validation results.
//!
//! The raw exchange (request bodies and response bodies, never the endpoint or credential) is kept
//! outside Git in the private evidence directory (`OHAND_PRIVATE_EVIDENCE_DIR`, default
//! `~/.ohand-private-evidence`, mode 0700 with a 0600 file). The sanitized artifact names that
//! file, its size and its SHA-256 so the private evidence can be audited against the record.

use super::{request_for, time_context};
use ohand_core::interpretation::instructions::{output_schema, InterpretationMapping};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, Clock, DispatchLimits, ProviderProfile, ProviderProfileBuilder,
    ProviderProtocol, SystemClock, TransportError,
};
use ohand_core::providers::openai::HttpTransport;
use ohand_core::providers::self_hosted::{
    declared_capabilities, SelfHostedAdapter, CHAT_VERIFIED_MODEL, MAX_COMPLETION_TOKENS,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const CONFIG_PATH_ENV_VAR: &str = "OHAND_PROVIDER_CONFIG_PATH";
const DEFAULT_CONFIG_PATH: &str = "/etc/ohand-provider/models.json";
const PROVIDER_KEY: &str = "ollama";
const EVIDENCE_DIR_ENV_VAR: &str = "OHAND_SMOKE_EVIDENCE_DIR";
const PRIVATE_EVIDENCE_DIR_ENV_VAR: &str = "OHAND_PRIVATE_EVIDENCE_DIR";
const DEFAULT_PRIVATE_EVIDENCE_DIRECTORY: &str = ".ohand-private-evidence";
const CREDENTIAL_REF: &str = "credential-ref/spark-endpoint";
const TIMEOUT_SECONDS: u32 = 120;
const SYNTHETIC_CAPTURES: [(&str, &str); 3] = [
    (
        "reminder-capture",
        "remind me to call the dentist tomorrow at 3pm",
    ),
    (
        "idea-capture",
        "idea: build a small birdhouse from scrap wood",
    ),
    (
        "plain-note-capture",
        "the garage door sticks when it is cold",
    ),
];

#[derive(Debug, Clone, Default)]
struct ObservedExchange {
    http_status: Option<u16>,
    response_bytes: usize,
    finish_reason: Option<String>,
    message_role: Option<String>,
    content_bytes: Option<usize>,
    curl_failure: Option<String>,
    raw_request: Vec<u8>,
    raw_response: Vec<u8>,
}

struct CurlTransport {
    base_credential: String,
    observed: Arc<Mutex<Vec<ObservedExchange>>>,
}

fn curl_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

impl HttpTransport for CurlTransport {
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        _deadline_ms: u64,
        _cancel: &CancelToken,
        _clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        assert_eq!(credential_ref, CREDENTIAL_REF);
        let body_file = tempfile::NamedTempFile::new().expect("temporary request body");
        std::fs::write(body_file.path(), &body).expect("write request body");

        let mut config = String::new();
        config.push_str(&format!("url = {}\n", curl_quoted(endpoint)));
        config.push_str(&format!(
            "header = {}\n",
            curl_quoted(&format!("Authorization: Bearer {}", self.base_credential))
        ));
        for (name, value) in headers {
            config.push_str(&format!(
                "header = {}\n",
                curl_quoted(&format!("{name}: {value}"))
            ));
        }
        config.push_str(&format!(
            "data-binary = {}\n",
            curl_quoted(&format!("@{}", body_file.path().display()))
        ));

        let mut child = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--max-time",
                &TIMEOUT_SECONDS.to_string(),
                "--write-out",
                "\n%{http_code}",
                "--config",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("curl is installed");
        child
            .stdin
            .take()
            .expect("curl stdin")
            .write_all(config.as_bytes())
            .expect("pass configuration to curl on stdin");
        let output = child.wait_with_output().expect("curl finishes");

        let mut exchange = ObservedExchange {
            raw_request: body.clone(),
            ..ObservedExchange::default()
        };
        if !output.status.success() {
            exchange.curl_failure =
                Some(format!("curl-exit-{}", output.status.code().unwrap_or(-1)));
            self.observed.lock().unwrap().push(exchange);
            return Err(if output.status.code() == Some(28) {
                TransportError::Timeout
            } else {
                TransportError::Unavailable
            });
        }
        let stdout = output.stdout;
        let split = stdout
            .iter()
            .rposition(|byte| *byte == b'\n')
            .expect("write-out status line");
        let status: u16 = std::str::from_utf8(&stdout[split + 1..])
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .expect("numeric status");
        let mut response = stdout[..split].to_vec();
        response.truncate(max_response_bytes as usize + 1);

        exchange.http_status = Some(status);
        exchange.response_bytes = response.len();
        exchange.raw_response = response.clone();
        if let Ok(envelope) = serde_json::from_slice::<Value>(&response) {
            let choice = &envelope["choices"][0];
            exchange.finish_reason = choice["finish_reason"].as_str().map(str::to_string);
            exchange.message_role = choice["message"]["role"].as_str().map(str::to_string);
            exchange.content_bytes = choice["message"]["content"].as_str().map(str::len);
        }
        self.observed.lock().unwrap().push(exchange);
        Ok((status, response))
    }
}

struct EndpointConfiguration {
    base_url: String,
    credential: String,
}

fn load_configuration() -> EndpointConfiguration {
    let path =
        std::env::var(CONFIG_PATH_ENV_VAR).unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("provider configuration is required at {path}"));
    let config: Value = serde_json::from_str(&raw).expect("provider configuration is JSON");
    let provider = &config["providers"][PROVIDER_KEY];
    EndpointConfiguration {
        base_url: provider["baseUrl"]
            .as_str()
            .expect("baseUrl")
            .trim()
            .to_string(),
        credential: provider["apiKey"]
            .as_str()
            .expect("apiKey")
            .trim()
            .to_string(),
    }
}

fn origin_of(base_url: &str) -> String {
    let after_scheme = base_url.find("://").expect("scheme") + 3;
    let end = base_url[after_scheme..]
        .find('/')
        .map_or(base_url.len(), |offset| after_scheme + offset);
    base_url[..end].to_string()
}

fn smoke_profile(configuration: &EndpointConfiguration) -> ProviderProfile {
    let mut builder =
        ProviderProfileBuilder::new("spark", ProviderProtocol::SelfHosted, CHAT_VERIFIED_MODEL)
            .endpoint(configuration.base_url.clone())
            .credential_ref(CREDENTIAL_REF)
            .timeout_seconds(TIMEOUT_SECONDS)
            .authorized_destination(origin_of(&configuration.base_url));
    for capability in declared_capabilities(CHAT_VERIFIED_MODEL, 2_000) {
        builder = builder.capability(capability);
    }
    builder
        .build()
        .expect("configured endpoint is a valid self-hosted profile")
}

/// Top-level conformance to `output_schema()`: required keys present, no key outside the
/// schema's properties (the schema sets `additionalProperties: false`).
fn conforms_at_top_level(proposal: &serde_json::Map<String, Value>) -> bool {
    let schema = output_schema();
    let properties = schema["properties"].as_object().expect("schema properties");
    let required = schema["required"].as_array().expect("schema required");
    required
        .iter()
        .all(|key| proposal.contains_key(key.as_str().unwrap()))
        && proposal.keys().all(|key| properties.contains_key(key))
}

fn private_evidence_directory(repository_root: &Path) -> PathBuf {
    let directory = match std::env::var(PRIVATE_EVIDENCE_DIR_ENV_VAR) {
        Ok(configured) => PathBuf::from(configured),
        Err(_) => PathBuf::from(std::env::var("HOME").expect("HOME locates the private directory"))
            .join(DEFAULT_PRIVATE_EVIDENCE_DIRECTORY),
    };
    std::fs::create_dir_all(&directory).expect("private evidence directory");
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("restrict private evidence directory");
    let resolved = directory.canonicalize().expect("resolve private directory");
    assert!(
        !resolved.starts_with(repository_root),
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

fn git_output(arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .output()
        .expect("git is available");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

#[test]
#[ignore = "needs the worker-mounted provider configuration and network access"]
fn live_endpoint_smoke_records_sanitized_evidence() {
    let requested_dir = PathBuf::from(
        std::env::var(EVIDENCE_DIR_ENV_VAR)
            .unwrap_or_else(|_| panic!("{EVIDENCE_DIR_ENV_VAR} must name the evidence directory")),
    );
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(requested_dir);
    let configuration = load_configuration();
    let scheme = configuration
        .base_url
        .split("://")
        .next()
        .unwrap()
        .to_string();
    let profile = smoke_profile(&configuration);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let transport = CurlTransport {
        base_credential: configuration.credential.clone(),
        observed: observed.clone(),
    };
    let adapter = SelfHostedAdapter::new(transport);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();

    let mut request_records = Vec::new();
    let mut raw_records = Vec::new();
    let mut failures = Vec::new();
    for (name, text) in SYNTHETIC_CAPTURES {
        let request = request_for(&profile, text);
        let started_ms = clock.now_ms();
        let result = dispatch(
            &adapter,
            &profile,
            &request,
            &clock,
            &cancel,
            &DispatchLimits::default(),
        );
        let elapsed_ms = clock.now_ms().saturating_sub(started_ms);
        let exchange: ObservedExchange = observed.lock().unwrap().pop().unwrap_or_default();

        raw_records.push(json!({
            "name": name,
            "http_status": exchange.http_status,
            "curl_failure": exchange.curl_failure,
            "request_body": String::from_utf8_lossy(&exchange.raw_request),
            "response_body": String::from_utf8_lossy(&exchange.raw_response),
        }));
        let mut record = json!({
            "name": name,
            "http_status": exchange.http_status,
            "curl_failure": exchange.curl_failure,
            "response_bytes": exchange.response_bytes,
            "finish_reason": exchange.finish_reason,
            "message_role": exchange.message_role,
            "content_bytes": exchange.content_bytes,
            "elapsed_ms": elapsed_ms,
        });
        match result {
            Ok(output) => {
                let conforms = conforms_at_top_level(&output.proposal);
                let mapping = InterpretationMapping::new(
                    &request,
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                )
                .expect("mapping binds the request");
                let proposal = mapping.map_output(&output);
                let validated = proposal
                    .as_ref()
                    .map(|candidate| candidate.validate(text).is_ok())
                    .unwrap_or(false);
                let mut keys: Vec<&String> = output.proposal.keys().collect();
                keys.sort();
                record["dispatch_result"] = json!("ok");
                record["proposal_keys"] = json!(keys);
                record["conforms_to_output_schema_top_level"] = json!(conforms);
                record["mapped_to_validated_proposal"] = json!(validated);
                if !conforms || !validated {
                    failures.push(format!("{name}: proposal-did-not-validate"));
                }
            }
            Err(failure) => {
                record["dispatch_result"] = json!(format!("{:?}", failure.kind));
                record["retriable"] = json!(failure.retriable);
                failures.push(format!("{name}: {:?}", failure.kind));
            }
        }
        request_records.push(record);
    }

    let dirty = !git_output(&[
        "status",
        "--porcelain",
        "--",
        "core",
        "Cargo.toml",
        "Cargo.lock",
    ])
    .is_empty();
    let commit = git_output(&["rev-parse", "HEAD"]);
    let collected_at = chrono::Utc::now();
    let evidence_id = format!(
        "self-hosted-adapter-smoke-{}-{}",
        collected_at.format("%Y%m%dT%H%M%SZ"),
        &commit[..8]
    );
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("repository root");
    let private_directory = private_evidence_directory(&repository_root);
    let raw_file_name = format!("{evidence_id}.raw.json");
    let mut raw_text = serde_json::to_string_pretty(&json!({
        "evidence_id": evidence_id,
        "collected_at": collected_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "adapter_revision": {"commit": commit, "dirty": dirty},
        "exchanges": raw_records,
    }))
    .unwrap();
    raw_text.push('\n');
    // The credential travels only in a header, so it can only appear in a request body by mistake.
    // It is not searched for in response bodies: a short placeholder credential (the verified
    // endpoint does not enforce one) can legitimately occur inside ordinary model output.
    for secret in [&configuration.base_url, &origin_of(&configuration.base_url)] {
        assert!(
            !raw_text.contains(secret.as_str()),
            "raw evidence must not carry the endpoint"
        );
    }
    assert!(
        raw_records.iter().all(|raw| !raw["request_body"]
            .as_str()
            .unwrap_or_default()
            .contains(configuration.credential.as_str())),
        "raw evidence must not carry the credential in a request body"
    );
    write_private_file(&private_directory.join(&raw_file_name), raw_text.as_bytes());
    let raw_sha256 = format!("{:x}", Sha256::digest(raw_text.as_bytes()));

    let artifact = json!({
        "schema_version": 1,
        "evidence_id": evidence_id,
        "collected_at": collected_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "collection_context": "odonian-worker",
        "adapter_revision": {"commit": commit, "dirty": dirty},
        "configured_scheme": scheme,
        "transport": "curl subprocess standing in for the native https transport; certificate validation on, redirects not followed",
        "model": CHAT_VERIFIED_MODEL,
        "max_tokens": MAX_COMPLETION_TOKENS,
        "request_shape": "adapter-built: interpretation prompt, strict json_schema response_format carrying output_schema()",
        "synthetic_context_time": time_context().reference_time.to_rfc3339(),
        "requests": request_records,
        "private_raw_evidence": {
            "run_id": evidence_id,
            "file_name": raw_file_name,
            "location": "private evidence directory of the collecting Odonian worker (OHAND_PRIVATE_EVIDENCE_DIR, default ~/.ohand-private-evidence), outside Git; mode 0600 in a 0700 directory",
            "bytes": raw_text.len(),
            "sha256": raw_sha256,
            "contents": "request and response bodies of every exchange; no endpoint address or credential",
        },
        "result": if failures.is_empty() { "pass" } else { "fail" },
        "failures": failures,
        "unverified": [
            "phone-context reachability of chat requests (owned by V08b)",
            "models other than the one named above",
            "endpoint credential validation (V08a: not enforced)"
        ],
    });
    std::fs::create_dir_all(&evidence_dir).expect("evidence directory");
    let path = evidence_dir.join(format!("{evidence_id}.json"));
    let mut text = serde_json::to_string_pretty(&artifact).unwrap();
    text.push('\n');
    for secret in [
        &configuration.credential,
        &configuration.base_url,
        &origin_of(&configuration.base_url),
    ] {
        assert!(
            !text.contains(secret.as_str()),
            "artifact must not carry endpoint or credential"
        );
    }
    std::fs::write(&path, text).expect("write artifact");
    println!("wrote {}", path.display());
    assert!(
        failures_empty(&artifact),
        "smoke run failed: {}",
        artifact["failures"]
    );
}

fn failures_empty(artifact: &Value) -> bool {
    artifact["result"] == "pass"
}
