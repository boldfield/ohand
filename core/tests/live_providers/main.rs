//! Live end-to-end provider harness (E02a).
//!
//! Runs every fixture in `fixtures/intent/contrastive-fixtures.json` through each configured
//! backend on the production path: ingress into a temporary store, profile and route grant
//! through the production configuration functions, the production adapter via
//! `InterpretationDispatcher`, and the production proposal application. It records the semantic
//! outcome against the fixture oracle, forbidden-mutation counts, latency and provider-reported
//! tokens, shows that switching backends leaves earlier captures byte-identical and earlier
//! results attributed to their own profile version, and asserts the failure behaviour of an
//! unreachable endpoint, an invalid hosted credential, a revoked profile version and a malformed
//! response.
//!
//! # Live run (ignored by default; needs network and credentials)
//!
//! ```text
//! OHAND_LIVE_PROFILES='self-hosted:<model>,anthropic:<model>,openai:<model>' \
//! OHAND_LIVE_CREDENTIAL_ENVS='anthropic=ANTHROPIC_API_KEY,openai=OPENAI_API_KEY' \
//! OHAND_PROVIDER_CONFIG_PATH=/path/to/models.json \
//! OHAND_LIVE_EVIDENCE_DIR=docs/validation/evidence/live-providers \
//! OHAND_PRIVATE_EVIDENCE_DIR=~/.ohand-private-evidence \
//!   cargo test --test live_providers live_providers_end_to_end -- --ignored --nocapture
//! ```
//!
//! Optional: `OHAND_LIVE_TIMEOUT_SECONDS` (1 to 300, default 300) and `OHAND_LIVE_RATES_FILE`
//! (JSON of per-million-token rates by backend label; cost is then written as a labelled
//! estimate). Hosted credentials are named by environment variable, never passed as values. The
//! self-hosted endpoint comes from provider key `ollama` of the file at
//! `OHAND_PROVIDER_CONFIG_PATH`, as in `providers_self_hosted/live_smoke.rs`. List at least two
//! profiles to exercise switching.
//!
//! The artifact in `OHAND_LIVE_EVIDENCE_DIR` carries no endpoint, credential, captured text or
//! model output text. Raw exchanges go to the private evidence directory (outside the
//! repository, mode 0700/0600); the artifact records that file's run id, name, size and SHA-256.
//! A leak check covers both and the harness refuses a private directory inside the repository.
//!
//! # No-network proof
//!
//! `no_network_proof_runs_the_whole_harness` runs the same code against a local TLS server
//! (needs `openssl`, `python3` and `curl`, all present on the CI runner). It is not ignored.

mod config;
mod evidence;
mod harness;
mod oracle;
mod tls_fixture;
mod transport;

use config::{Backend, LiveConfig, Protocol, EVIDENCE_DIR_ENV_VAR};
use evidence::{
    assert_no_leaks, ensure_private_directory, private_evidence_directory, repository_root,
    write_evidence, EvidenceInputs,
};
use harness::{build_info, Harness, ScenarioInputs};
use oracle::{load_fixtures, OracleFixture};
use serde_json::{json, Value};
use std::path::PathBuf;
use tls_fixture::{
    TlsFixture, FIXTURE_CREDENTIAL, MODEL_OUTPUT_MARKER, SELF_HOSTED_BASE_URL, SELF_HOSTED_HOST,
    SELF_HOSTED_PORT,
};
use transport::{CurlTransport, TestRouting};

fn run_id(context: &str) -> String {
    let build = build_info();
    format!(
        "live-providers-{context}-{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
        build.commit.chars().take(70).collect::<String>()
    )
}

/// A proposal that agrees with the fixture's oracle where the response schema allows it.
fn oracle_proposal(fixture: &OracleFixture) -> Value {
    let mut proposal = json!({"operation": {"kind": "annotate"}});
    if let Some(item_type) = fixture.expected["item_type"].as_str() {
        proposal["item_type"] = json!(item_type);
    }
    let span = |span: &Value| json!({"start": span["start"], "end": span["end"]});
    if let Some(spans) = fixture.expected["source_spans"].as_array() {
        proposal["source_spans"] = json!(spans.iter().map(span).collect::<Vec<_>>());
    }
    let reminder = &fixture.expected["reminder_proposal"];
    if !reminder.is_null() {
        let mut candidate = json!({
            "quality": reminder["quality"],
            "source_span": span(&reminder["source_span"]),
        });
        for key in ["instant", "timezone_id"] {
            if !reminder[key].is_null() {
                candidate[key] = reminder[key].clone();
            }
        }
        proposal["reminder_proposal"] = candidate;
    }
    let topic = &fixture.expected["session_topic_proposal"];
    if !topic.is_null() {
        proposal["session_topic_proposal"] =
            json!({"topic": topic["topic"], "source_span": span(&topic["source_span"])});
    }
    if !fixture.expected["abstention"].is_null() {
        proposal["abstention"] = match &fixture.expected["abstention"] {
            Value::String(reason) => json!({"reason": reason}),
            other => json!({"reason": "Other", "detail": other["Other"]}),
        };
    }
    proposal
}

fn fixture_backends() -> Vec<Backend> {
    vec![
        Backend {
            protocol: Protocol::SelfHosted,
            model: "fixture-self-hosted-model".to_string(),
            endpoint: Some(SELF_HOSTED_BASE_URL.to_string()),
            credential: FIXTURE_CREDENTIAL.to_string(),
        },
        Backend {
            protocol: Protocol::Anthropic,
            model: "fixture-anthropic-model".to_string(),
            endpoint: None,
            credential: FIXTURE_CREDENTIAL.to_string(),
        },
    ]
}

#[test]
fn no_network_proof_runs_the_whole_harness() {
    let server = TlsFixture::start();
    let transport = CurlTransport::new(
        30,
        TestRouting {
            ca_file: Some(server.ca_file()),
            connect_to: vec![
                (SELF_HOSTED_HOST.to_string(), SELF_HOSTED_PORT, server.port),
                ("api.anthropic.com".to_string(), 443, server.port),
                ("api.openai.com".to_string(), 443, server.port),
            ],
        },
    );
    let mut harness = Harness::new(transport, 30);
    let backends = fixture_backends();
    let fixtures = load_fixtures();

    let report = harness.run_backends(&backends, &fixtures, &mut |_, fixture| {
        server.set_reply(&oracle_proposal(fixture));
    });

    assert!(
        report.configure_failures.is_empty(),
        "{:?}",
        report.configure_failures
    );
    assert_eq!(report.records.len(), fixtures.len() * backends.len());
    for backend in &backends {
        let own: Vec<_> = report
            .records
            .iter()
            .filter(|record| record.backend == backend.label())
            .collect();
        // The production path and apply: provider calls were made and proposals were applied.
        let provider_applied = own
            .iter()
            .filter(|record| {
                record.run.outcome.is_applied()
                    && record.run.outcome.route.as_deref() == Some("provider")
            })
            .count();
        assert!(
            provider_applied >= 10,
            "{}: {provider_applied}",
            backend.label()
        );
        for record in &own {
            // Latency and provider-reported tokens accompany every provider call.
            if record.run.outcome.route.as_deref() == Some("provider") {
                assert!(!record.run.exchanges.is_empty());
                let (input, output) = record.tokens();
                assert!(input.is_some() && output.is_some());
            }
            // Never a forbidden mutation, whatever the model said.
            assert_eq!(record.run.forbidden.total(), 0, "{}", record.fixture_id);
            assert!(record.run.capture_text_durable);
        }
        // The oracle-following server produces matches the scorer recognizes.
        assert!(own.iter().filter(|r| r.matches_oracle()).count() >= 10);
    }

    // Switching: backend A's captures, results and attribution survive backend B.
    assert_eq!(report.switching.len(), 1);
    assert!(report.switching[0].holds(), "{:?}", report.switching[0]);
    let first_versions: std::collections::BTreeSet<_> = report
        .records
        .iter()
        .filter(|record| record.backend == backends[0].label())
        .filter_map(|record| record.profile_version.clone())
        .collect();
    let second_versions: std::collections::BTreeSet<_> = report
        .records
        .iter()
        .filter(|record| record.backend == backends[1].label())
        .filter_map(|record| record.profile_version.clone())
        .collect();
    assert_eq!(first_versions.len(), 1);
    assert_eq!(second_versions.len(), 1);
    assert!(first_versions.is_disjoint(&second_versions));

    // Every failure class, each asserted inside the scenario and again here.
    let served_before = server.requests().len();
    let scenarios = harness.run_failure_scenarios(
        &ScenarioInputs {
            primary: &backends[0],
            hosted: Some(&backends[1]),
            malformed: backends.iter().collect(),
        },
        &mut |name| {
            server.set_mode(if name == "malformed" {
                "malformed"
            } else {
                "ok"
            })
        },
    );
    let names: Vec<_> = scenarios.iter().map(|s| s.scenario.as_str()).collect();
    assert_eq!(
        names,
        [
            "unreachable_endpoint",
            "invalid_hosted_credential",
            "revoked_profile_version",
            "malformed_response",
            "malformed_response"
        ]
    );
    for scenario in &scenarios {
        assert_eq!(scenario.status, "ran", "{scenario:?}");
        assert!(scenario.passed, "{scenario:?}");
        assert_eq!(scenario.applied_proposals, 0);
        assert!(scenario.capture_durable);
        assert!(scenario.classified_retriable.is_some());
    }
    assert_eq!(scenarios[0].classified_retriable, Some(true));
    assert_eq!(scenarios[1].classified_retriable, Some(false));
    assert_eq!(scenarios[2].provider_calls, 0);
    assert_eq!(scenarios[3].classified_retriable, Some(false));
    // Unreachable and revoked never reached the server; the other three did, once each.
    assert_eq!(server.requests().len() - served_before, 3);

    // Evidence: sanitized artifact and private raw file, both leak-checked.
    let scratch = tempfile::tempdir().unwrap();
    let artifact_dir = scratch.path().join("artifacts");
    let private_dir = ensure_private_directory(&scratch.path().join("private"));
    let id = run_id("ci");
    let written = write_evidence(
        &EvidenceInputs {
            run_id: &id,
            collection_context: "ci-no-network-proof",
            backends: &backends,
            fixtures: &fixtures,
            report: &report,
            scenarios: &scenarios,
            timeout_seconds: 30,
        },
        &artifact_dir,
        &private_dir,
        &[MODEL_OUTPUT_MARKER.to_string()],
    );
    for secret in [
        FIXTURE_CREDENTIAL,
        SELF_HOSTED_HOST,
        MODEL_OUTPUT_MARKER,
        "api.anthropic.com",
    ] {
        assert!(
            !written.artifact_text.contains(secret),
            "artifact leaked {secret}"
        );
    }
    assert!(
        written.raw_text.contains(MODEL_OUTPUT_MARKER),
        "raw keeps the exchange"
    );
    assert!(!written.raw_text.contains(SELF_HOSTED_HOST));
    let artifact: Value = serde_json::from_str(&written.artifact_text).unwrap();
    let raw_reference = &artifact["private_raw_evidence"];
    assert_eq!(raw_reference["run_id"], id);
    assert_eq!(raw_reference["bytes"], written.raw_text.len());
    assert_eq!(
        raw_reference["sha256"],
        format!("{:x}", sha2_digest(written.raw_text.as_bytes()))
    );
    assert_eq!(
        std::fs::read_to_string(&written.raw_path).unwrap(),
        written.raw_text
    );
    assert_eq!(artifact["switching"][0]["captures_changed"], 0);
    assert_eq!(artifact["cost"], "not computed: no rates file supplied");
}

fn sha2_digest(bytes: &[u8]) -> impl std::fmt::LowerHex {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
}

#[test]
#[should_panic(expected = "must not carry a forbidden value")]
fn leak_check_rejects_a_credential_in_an_artifact() {
    assert_no_leaks(
        "artifact",
        "{\"note\": \"fixture-credential-7f3a9c1e5b2d4468\"}",
        &[FIXTURE_CREDENTIAL.to_string()],
    );
}

fn raw_exchange(request_body: &str, response_body: &str) -> serde_json::Value {
    serde_json::json!({"request_body": request_body, "response_body": response_body})
}

#[test]
#[should_panic(expected = "must not carry a forbidden value")]
fn leak_check_rejects_a_credential_in_raw_request_data() {
    let exchanges = vec![raw_exchange(&format!("echo {FIXTURE_CREDENTIAL}"), "ok")];
    let raw_text = serde_json::to_string(&exchanges).unwrap();
    evidence::assert_raw_clean(&exchanges, &raw_text, &fixture_backends());
}

#[test]
#[should_panic(expected = "must not carry a forbidden value")]
fn leak_check_rejects_a_scenario_credential_in_raw_request_data() {
    let exchanges = vec![raw_exchange(harness::INVALID_SCENARIO_CREDENTIAL, "ok")];
    let raw_text = serde_json::to_string(&exchanges).unwrap();
    evidence::assert_raw_clean(&exchanges, &raw_text, &fixture_backends());
}

#[test]
fn leak_check_accepts_a_credential_echoed_in_a_response_body() {
    let exchanges = vec![raw_exchange("{}", &format!("fp_{FIXTURE_CREDENTIAL}"))];
    let raw_text = serde_json::to_string(&exchanges).unwrap();
    evidence::assert_raw_clean(&exchanges, &raw_text, &fixture_backends());
}

#[test]
fn a_transient_failure_stays_with_its_own_fixture_and_switching_holds() {
    let server = TlsFixture::start();
    let transport = CurlTransport::new(
        30,
        TestRouting {
            ca_file: Some(server.ca_file()),
            connect_to: vec![
                (SELF_HOSTED_HOST.to_string(), SELF_HOSTED_PORT, server.port),
                ("api.anthropic.com".to_string(), 443, server.port),
            ],
        },
    );
    let mut harness = Harness::new(transport, 30);
    let backends = fixture_backends();
    let fixtures: Vec<OracleFixture> = load_fixtures().into_iter().take(4).collect();
    let flaky = fixtures[1].id.clone();

    let report = harness.run_backends(&backends, &fixtures, &mut |backend, fixture| {
        server.set_reply(&oracle_proposal(fixture));
        let first_backend = backend.label() == backends[0].label();
        server.set_mode(if first_backend && fixture.id == flaky {
            "unavailable_once"
        } else {
            "ok"
        });
    });

    assert!(report.configure_failures.is_empty());
    let record_of = |backend: &Backend, fixture: &OracleFixture| {
        report
            .records
            .iter()
            .find(|record| record.backend == backend.label() && record.fixture_id == fixture.id)
            .expect("record")
    };
    let flaky_record = record_of(&backends[0], &fixtures[1]);
    // Its own retry is inside its own record: a 503 followed by a success, one final outcome.
    assert_eq!(
        flaky_record.run.exchanges.len(),
        2,
        "{:?}",
        flaky_record.run.outcome
    );
    assert_eq!(flaky_record.run.outcome.result, "applied");
    assert_eq!(flaky_record.run.job_status.as_deref(), Some("completed"));
    for fixture in [&fixtures[0], &fixtures[2], &fixtures[3]] {
        for backend in &backends {
            let record = record_of(backend, fixture);
            if record.run.outcome.route.as_deref() == Some("provider") {
                assert_eq!(record.run.exchanges.len(), 1, "{}", record.fixture_id);
            }
            assert_eq!(record.run.forbidden.total(), 0);
        }
    }
    assert!(report.switching[0].holds(), "{:?}", report.switching[0]);
}

#[test]
#[should_panic(expected = "outside the repository")]
fn private_directory_inside_the_repository_is_refused() {
    ensure_private_directory(
        &repository_root()
            .join("target")
            .join("live-private-refusal-check"),
    );
}

#[test]
fn a_credential_is_refused_toward_an_origin_it_was_not_bound_to() {
    let transport = CurlTransport::new(5, TestRouting::default());
    transport.bind_credential("ref-1", "bound-secret-value", "https://bound.example");
    let adapter_transport = transport::SharedTransport(transport.clone());
    let clock = ohand_core::providers::contracts::ManualClock::new();
    let result = ohand_core::providers::openai::HttpTransport::post(
        &adapter_transport,
        "https://other.example/v1/chat/completions",
        "ref-1",
        &[],
        Vec::new(),
        10_000,
        &ohand_core::providers::contracts::CancelToken::new(),
        &clock,
        1024,
    );
    assert_eq!(
        result,
        Err(ohand_core::providers::contracts::TransportError::Rejected)
    );
    let exchanges = transport.drain_exchanges();
    assert_eq!(
        exchanges[0].transport_failure.as_deref(),
        Some("credential-not-bound-to-origin")
    );
}

#[test]
#[ignore = "needs network access and provider credentials; see the module header"]
fn live_providers_end_to_end() {
    let config = LiveConfig::from_env().unwrap_or_else(|reason| panic!("{reason}"));
    let evidence_dir = repository_root().join(PathBuf::from(
        std::env::var(EVIDENCE_DIR_ENV_VAR)
            .unwrap_or_else(|_| panic!("{EVIDENCE_DIR_ENV_VAR} must name the evidence directory")),
    ));
    let private_dir = private_evidence_directory();
    let fixtures = load_fixtures();
    let mut harness = Harness::new(
        CurlTransport::new(config.timeout_seconds, TestRouting::default()),
        config.timeout_seconds,
    );

    let report = harness.run_backends(&config.backends, &fixtures, &mut |_, _| {});
    let hosted = config.backends.iter().find(|b| b.protocol.is_hosted());
    let scenarios = harness.run_failure_scenarios(
        &ScenarioInputs {
            primary: &config.backends[0],
            hosted,
            malformed: Vec::new(),
        },
        &mut |_| {},
    );
    let id = run_id("live");
    let written = write_evidence(
        &EvidenceInputs {
            run_id: &id,
            collection_context: "coordinator-live-run",
            backends: &config.backends,
            fixtures: &fixtures,
            report: &report,
            scenarios: &scenarios,
            timeout_seconds: config.timeout_seconds,
        },
        &evidence_dir,
        &private_dir,
        &[],
    );
    println!("wrote {}", written.artifact_path.display());

    assert!(
        report.configure_failures.is_empty(),
        "{:?}",
        report.configure_failures
    );
    for record in &report.records {
        assert_eq!(record.run.forbidden.total(), 0, "{}", record.fixture_id);
    }
    for check in &report.switching {
        assert!(check.holds(), "{check:?}");
    }
    for scenario in &scenarios {
        assert!(scenario.passed, "{scenario:?}");
    }
}
