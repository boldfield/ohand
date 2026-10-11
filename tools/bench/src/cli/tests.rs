//! End-to-end tests of the command: argument handling, the dry run, fake-mode runs with resume
//! and rerun, every live-mode refusal, a live run against a local TLS fixture server, and
//! cancellation. Every corpus is the committed synthetic fixture file; nothing reaches a real
//! provider, and the secrets here are made-up strings.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ohand_core::providers::contracts::CancelToken;
use serde_json::{json, Value};
use tempfile::TempDir;

// The transport tests own this fixture server privately; it is included by path here rather than
// widening its visibility outside this command.
#[path = "../transport/fixture.rs"]
#[allow(dead_code, clippy::duplicate_mod)]
mod fixture;

use super::{main_with, Services, EXIT_COMPLETED, EXIT_INCOMPLETE, EXIT_REFUSED, USAGE};
use crate::journal::JournalReader;
use fixture::{Fixture, RecordedRequest, Reply, TestAuthority};

const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/intent/contrastive-fixtures.json"
);
const SECRET_A: &str = "synthetic-secret-alpha-0001";
const SECRET_B: &str = "synthetic-secret-bravo-0002";

struct Outcome {
    status: i32,
    out: String,
    err: String,
}

fn services_with(variables: &[(&str, &str)]) -> Services {
    let variables: HashMap<OsString, OsString> = variables
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    Services {
        environment: Arc::new(move |name| variables.get(name).cloned()),
        anthropic_endpoint: None,
        openai_endpoint: None,
        extra_trust_roots: Vec::new(),
        cancel: CancelToken::new(),
    }
}

fn execute(words: &[&str], services: &Services) -> Outcome {
    let words: Vec<String> = words.iter().map(|word| word.to_string()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = main_with(&words, services, &mut out, &mut err);
    Outcome {
        status,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn fake_words<'a>(journal: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = vec![
        "run",
        "--corpus",
        CORPUS,
        "--profile-a",
        "anthropic:model-a",
        "--profile-b",
        "openai:model-b",
        "--journal",
        journal,
    ];
    if !extra.contains(&"--max-cases") {
        words.extend(["--max-cases", "3"]);
    }
    words.extend_from_slice(extra);
    words
}

fn journal_in(dir: &TempDir) -> (PathBuf, String) {
    let path = dir.path().join("run.ndjson");
    let text = path.to_str().unwrap().to_string();
    (path, text)
}

/// Attempts that were really dispatched; sides skipped by the budget or a cancel are journaled
/// with `dispatched: false` and do not count.
fn attempt_count(path: &Path) -> usize {
    let reader = JournalReader::open(path).unwrap();
    let mut ids: Vec<&str> = reader
        .attempts()
        .iter()
        .filter(|attempt| {
            attempt
                .provider_output
                .as_ref()
                .is_some_and(|output| output["dispatched"] == json!(true))
        })
        .map(|attempt| attempt.id.as_str())
        .collect();
    ids.sort();
    ids.dedup();
    ids.len()
}

fn first_case_id() -> String {
    nth_case_id(0)
}

fn nth_case_id(index: usize) -> String {
    let corpus: Value = serde_json::from_str(&std::fs::read_to_string(CORPUS).unwrap()).unwrap();
    let fixtures = corpus
        .get("fixtures")
        .or_else(|| corpus.get("cases"))
        .and_then(Value::as_array)
        .expect("corpus lists fixtures");
    fixtures[index]["id"].as_str().unwrap().to_string()
}

// ---- help and argument handling -------------------------------------------------------------

#[test]
fn help_prints_usage_without_credentials_or_files() {
    let outcome = execute(&["help"], &services_with(&[]));
    assert_eq!(outcome.status, EXIT_COMPLETED);
    assert_eq!(outcome.out, USAGE);
    assert!(outcome.err.is_empty());
    for word in [
        "--execute-network",
        "--credential-env",
        "--allow-destination",
    ] {
        assert!(outcome.out.contains(word), "usage documents {word}");
    }
}

#[test]
fn no_command_and_unknown_command_are_refused() {
    for words in [vec![], vec!["frobnicate"]] {
        let outcome = execute(&words, &services_with(&[]));
        assert_eq!(outcome.status, EXIT_REFUSED, "{words:?}");
        assert!(outcome.out.is_empty());
    }
}

#[test]
fn malformed_invocations_are_refused_with_no_output_on_stdout() {
    let journal = "/nonexistent/never-created.ndjson";
    // Assembled at runtime so no key-shaped literal is committed to the repository.
    let key_shaped_reference = format!("anthropic:m1:{}{}", "sk-ant-", "api03-abcdefghijklmnop");
    let cases: Vec<Vec<&str>> = vec![
        // unknown flag
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--bogus",
        ],
        // only one profile
        vec!["plan", "--corpus", CORPUS, "--profile-a", "anthropic:m1"],
        // three profiles
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--profile-b",
            "openai:m3",
        ],
        // no corpus
        vec![
            "plan",
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
        ],
        // identical profiles
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "anthropic:m1",
        ],
        // self-hosted is not supported
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "self-hosted:m1",
            "--profile-b",
            "openai:m2",
        ],
        // protocol typo
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "acme:m1",
            "--profile-b",
            "openai:m2",
        ],
        // plan never executes
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--mode",
            "live",
            "--execute-network",
        ],
        // network switches outside live mode
        vec![
            "run",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--journal",
            journal,
            "--execute-network",
        ],
        vec![
            "run",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--journal",
            journal,
            "--credential-env",
            "bench-anthropic=SOME_VAR",
        ],
        vec![
            "run",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--journal",
            journal,
            "--allow-destination",
            "anthropic",
        ],
        // run needs a journal
        vec![
            "run",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
        ],
        // out-of-range numbers
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--attempts-per-side",
            "6",
        ],
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
            "--timeout-seconds",
            "0",
        ],
        // a secret-looking credential reference is refused rather than echoed
        vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            &key_shaped_reference,
            "--profile-b",
            "openai:m2",
        ],
    ];
    for words in cases {
        let outcome = execute(&words, &services_with(&[]));
        assert_eq!(outcome.status, EXIT_REFUSED, "{words:?}\n{}", outcome.err);
        assert!(outcome.out.is_empty(), "{words:?}");
        assert!(!outcome.err.contains("sk-ant"), "{words:?}");
    }
    assert!(!Path::new(journal).exists());
}

#[test]
fn unreadable_or_invalid_corpus_is_refused_without_a_journal() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let invalid = dir.path().join("invalid.json");
    std::fs::write(&invalid, "{\"not\": \"a corpus\"}").unwrap();
    for corpus in [dir.path().join("absent.json"), invalid] {
        let corpus = corpus.to_str().unwrap();
        let outcome = execute(
            &[
                "run",
                "--corpus",
                corpus,
                "--profile-a",
                "anthropic:m1",
                "--profile-b",
                "openai:m2",
                "--journal",
                &journal_text,
            ],
            &services_with(&[]),
        );
        assert_eq!(outcome.status, EXIT_REFUSED);
        assert!(!journal.exists());
    }
}

#[test]
fn unknown_case_and_duplicate_case_are_refused() {
    let first = first_case_id();
    for case_args in [
        vec!["--case", "no-such-fixture"],
        vec!["--case", &first, "--case", &first],
    ] {
        let mut words = vec![
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:m1",
            "--profile-b",
            "openai:m2",
        ];
        words.extend(case_args);
        let outcome = execute(&words, &services_with(&[]));
        assert_eq!(outcome.status, EXIT_REFUSED, "{}", outcome.err);
    }
}

// ---- dry run --------------------------------------------------------------------------------

#[test]
fn plan_over_the_real_corpus_lists_attempts_and_touches_nothing() {
    let dir = TempDir::new().unwrap();
    let outcome = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:model-a",
            "--profile-b",
            "openai:model-b",
        ],
        &services_with(&[]),
    );
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(outcome.out.contains("dry run"));
    assert!(outcome.out.contains("mode: fake"));
    assert!(outcome.out.contains("planned requests:"));
    assert!(outcome.out.contains("anthropic / model-a"));
    assert!(outcome.out.contains("openai / model-b"));
    assert!(outcome.out.contains("to send"));
    assert!(outcome.out.contains("build revision: bench-"));
    assert!(outcome.err.is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn plan_does_not_read_any_environment_variable() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = reads.clone();
    let mut services = services_with(&[]);
    services.environment = Arc::new(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    });
    for mode in ["fake", "live"] {
        let outcome = execute(
            &[
                "plan",
                "--corpus",
                CORPUS,
                "--profile-a",
                "anthropic:model-a",
                "--profile-b",
                "openai:model-b",
                "--mode",
                mode,
            ],
            &services,
        );
        assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    }
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn live_plan_names_what_a_live_run_still_needs_and_labels_the_mode() {
    let outcome = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:model-a",
            "--profile-b",
            "openai:model-b",
            "--mode",
            "live",
            "--max-cases",
            "2",
        ],
        &services_with(&[]),
    );
    assert_eq!(outcome.status, EXIT_COMPLETED);
    assert!(outcome.out.contains("mode: live"));
    assert!(outcome.out.contains("--execute-network"));
    assert!(!outcome.out.contains("fake mode results"));
}

#[test]
fn plan_against_an_existing_journal_shows_kept_sides_and_does_not_modify_it() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let first = execute(&fake_words(&journal_text, &["--max-cases", "2"]), &services);
    assert_eq!(first.status, EXIT_COMPLETED, "{}", first.err);
    let before = std::fs::read(&journal).unwrap();

    let outcome = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:model-a",
            "--profile-b",
            "openai:model-b",
            "--journal",
            &journal_text,
            "--max-cases",
            "3",
        ],
        &services,
    );
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(outcome.out.contains("kept"), "{}", outcome.out);
    assert!(outcome.out.contains("to send"));
    assert_eq!(std::fs::read(&journal).unwrap(), before);
}

#[test]
fn plan_with_a_different_model_than_the_journal_reports_the_configuration_change() {
    let dir = TempDir::new().unwrap();
    let (_, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &[]), &services).status,
        EXIT_COMPLETED
    );
    let outcome = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "anthropic:model-changed",
            "--profile-b",
            "openai:model-b",
            "--journal",
            &journal_text,
            "--max-cases",
            "3",
        ],
        &services,
    );
    assert!(
        outcome.out.contains("differs") || outcome.out.contains("change"),
        "{}",
        outcome.out
    );
}

// ---- fake-mode runs -------------------------------------------------------------------------

#[test]
fn fake_run_completes_records_mode_and_revisions_and_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let outcome = execute(
        &fake_words(&journal_text, &["--build-revision", "test-build-7"]),
        &services_with(&[]),
    );
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(outcome.out.contains("mode: fake"));
    assert!(outcome.out.contains("build revision: test-build-7"));
    assert!(outcome.out.contains("corpus: sha256:"));
    assert!(outcome.out.contains("diagnostic only"));
    assert!(outcome.out.contains("validate the pipeline only"));
    assert!(outcome.out.contains("completed"));
    assert!(!outcome.out.contains("synthetic fake reply"));
    assert_eq!(attempt_count(&journal), 6);

    let text = std::fs::read_to_string(&journal).unwrap();
    assert!(text.contains("mode-fake"));
    assert!(text.contains("test-build-7"));
    assert!(!text.contains("source_revision"));
}

#[test]
fn fake_run_without_a_build_revision_marks_the_source_revision_unknown() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let outcome = execute(&fake_words(&journal_text, &[]), &services_with(&[]));
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(std::fs::read_to_string(&journal)
        .unwrap()
        .contains("source_revision"));
}

#[test]
fn rerunning_the_same_command_sends_nothing_new() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &[]), &services).status,
        EXIT_COMPLETED
    );
    let second = execute(&fake_words(&journal_text, &[]), &services);
    assert_eq!(second.status, EXIT_COMPLETED, "{}", second.err);
    assert!(
        second.out.contains("requests reserved this run: 0"),
        "{}",
        second.out
    );
    assert_eq!(attempt_count(&journal), 6);
}

#[test]
fn a_larger_run_over_the_same_journal_only_sends_the_new_cases() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &["--max-cases", "2"]), &services).status,
        EXIT_COMPLETED
    );
    assert_eq!(attempt_count(&journal), 4);
    let grown = execute(&fake_words(&journal_text, &["--max-cases", "4"]), &services);
    assert_eq!(grown.status, EXIT_COMPLETED, "{}", grown.err);
    assert_eq!(attempt_count(&journal), 8);
}

#[test]
fn explicit_rerun_sends_exactly_that_side_again() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &[]), &services).status,
        EXIT_COMPLETED
    );
    let target = format!("{}:a", first_case_id());
    let rerun = execute(&fake_words(&journal_text, &["--rerun", &target]), &services);
    assert_eq!(rerun.status, EXIT_COMPLETED, "{}", rerun.err);
    assert_eq!(attempt_count(&journal), 7);
}

#[test]
fn refused_rerun_on_a_fresh_journal_creates_no_file_and_pins_nothing() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let target = format!("{}:a", first_case_id());
    let refused = execute(
        &fake_words(
            &journal_text,
            &["--rerun", &target, "--build-revision", "build-1"],
        ),
        &services,
    );
    assert_eq!(refused.status, EXIT_REFUSED, "{}", refused.err);
    assert!(
        refused.err.contains("no dispatched attempt"),
        "{}",
        refused.err
    );
    assert!(
        !journal.exists(),
        "a refused run must not create the journal"
    );

    let corrected = execute(
        &fake_words(&journal_text, &["--build-revision", "build-2"]),
        &services,
    );
    assert_eq!(corrected.status, EXIT_COMPLETED, "{}", corrected.err);
    assert_eq!(attempt_count(&journal), 6);
}

#[test]
fn refused_rerun_leaves_an_existing_journal_byte_identical() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &["--max-cases", "2"]), &services).status,
        EXIT_COMPLETED
    );
    let before = std::fs::read(&journal).unwrap();

    let unplanned = execute(
        &fake_words(
            &journal_text,
            &["--max-cases", "2", "--rerun", "no-such-case:a"],
        ),
        &services,
    );
    assert_eq!(unplanned.status, EXIT_REFUSED, "{}", unplanned.err);
    assert!(
        unplanned.err.contains("not in the plan"),
        "{}",
        unplanned.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);

    // Planned by --max-cases 3 but never dispatched in the two-case run.
    let target = format!("{}:a", nth_case_id(2));
    let undispatched = execute(&fake_words(&journal_text, &["--rerun", &target]), &services);
    assert_eq!(undispatched.status, EXIT_REFUSED, "{}", undispatched.err);
    assert!(
        undispatched.err.contains("no dispatched attempt"),
        "{}",
        undispatched.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);
}

#[test]
fn request_cap_stops_the_run_and_exits_incomplete() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let capped = execute(
        &fake_words(&journal_text, &["--request-cap", "2"]),
        &services,
    );
    assert_eq!(capped.status, EXIT_INCOMPLETE, "{}", capped.err);
    assert!(
        capped.out.contains("budget exhausted: yes"),
        "{}",
        capped.out
    );
    assert_eq!(attempt_count(&journal), 2);

    let resumed = execute(&fake_words(&journal_text, &[]), &services);
    assert_eq!(resumed.status, EXIT_COMPLETED, "{}", resumed.err);
    assert_eq!(attempt_count(&journal), 6);
}

#[test]
fn changed_configuration_is_refused_and_leaves_the_journal_untouched() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(
            &fake_words(&journal_text, &["--build-revision", "build-1"]),
            &services
        )
        .status,
        EXIT_COMPLETED
    );
    let before = std::fs::read(&journal).unwrap();

    let changed_build = execute(
        &fake_words(&journal_text, &["--build-revision", "build-2"]),
        &services,
    );
    assert_eq!(changed_build.status, EXIT_REFUSED, "{}", changed_build.out);
    assert!(changed_build.out.is_empty());

    let mut words = fake_words(&journal_text, &["--build-revision", "build-1"]);
    let position = words
        .iter()
        .position(|word| *word == "openai:model-b")
        .unwrap();
    words[position] = "openai:model-other";
    let changed_model = execute(&words, &services);
    assert_eq!(changed_model.status, EXIT_REFUSED);
    assert_eq!(std::fs::read(&journal).unwrap(), before);
}

#[test]
fn recorded_mode_is_refused_for_plan_and_run() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    for command in ["plan", "run"] {
        let outcome = execute(
            &[
                command,
                "--corpus",
                CORPUS,
                "--profile-a",
                "anthropic:model-a",
                "--profile-b",
                "openai:model-b",
                "--mode",
                "recorded",
                "--journal",
                &journal_text,
            ],
            &services_with(&[]),
        );
        assert_eq!(outcome.status, EXIT_REFUSED, "{command}");
        assert!(outcome.err.contains("recorded"), "{}", outcome.err);
    }
    assert!(!journal.exists());
}

// ---- live-mode refusals ---------------------------------------------------------------------

fn live_words<'a>(journal: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = vec![
        "run",
        "--corpus",
        CORPUS,
        "--profile-a",
        "anthropic:model-a:ref-a",
        "--profile-b",
        "anthropic:model-b:ref-b",
        "--mode",
        "live",
        "--journal",
        journal,
        "--max-cases",
        "2",
    ];
    words.extend_from_slice(extra);
    words
}

const GRANTS: [&str; 6] = [
    "--allow-destination",
    "anthropic",
    "--credential-env",
    "ref-a=BENCH_TEST_KEY_A",
    "--credential-env",
    "ref-b=BENCH_TEST_KEY_B",
];

fn assert_refused_before_anything_was_sent(
    words: &[&str],
    services: &Services,
    journal: &Path,
    server: &Fixture,
) -> Outcome {
    let outcome = execute(words, services);
    assert_eq!(outcome.status, EXIT_REFUSED, "{}", outcome.err);
    assert!(outcome.out.is_empty());
    assert!(outcome.err.contains("nothing was sent"));
    assert!(!journal.exists(), "the journal is not opened on a refusal");
    assert_eq!(server.connections(), 0);
    for secret in [SECRET_A, SECRET_B, "BENCH_TEST_KEY"] {
        assert!(!outcome.err.contains(secret) && !outcome.out.contains(secret));
    }
    outcome
}

fn silent_server(authority: &TestAuthority) -> Fixture {
    Fixture::start(authority.issue("localhost"), |_| Reply::status(500, b"{}"))
}

fn fixture_services(
    authority: &TestAuthority,
    server: &Fixture,
    variables: &[(&str, &str)],
) -> Services {
    let mut services = services_with(variables);
    services.anthropic_endpoint = Some(server.url("/v1/messages"));
    services.extra_trust_roots = vec![authority.root_der()];
    services
}

#[test]
fn live_refusals_happen_before_the_journal_or_any_connection() {
    let authority = TestAuthority::new();
    let server = silent_server(&authority);
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let both = [
        ("BENCH_TEST_KEY_A", SECRET_A),
        ("BENCH_TEST_KEY_B", SECRET_B),
    ];

    // no --execute-network
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend(["--request-cap", "4"]);
    let services = fixture_services(&authority, &server, &both);
    let outcome = assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &extra),
        &services,
        &journal,
        &server,
    );
    assert!(outcome.err.contains("--execute-network"));

    // no explicit request cap
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.push("--execute-network");
    let outcome = assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &extra),
        &services,
        &journal,
        &server,
    );
    assert!(outcome.err.contains("--request-cap"));

    // destination not granted
    let extra = [
        "--execute-network",
        "--request-cap",
        "4",
        "--credential-env",
        "ref-a=BENCH_TEST_KEY_A",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
    ];
    let outcome = assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &extra),
        &services,
        &journal,
        &server,
    );
    assert!(outcome.err.contains("--allow-destination"));

    // a grant for a protocol no profile uses
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend([
        "--execute-network",
        "--request-cap",
        "4",
        "--allow-destination",
        "openai",
    ]);
    assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &extra),
        &services,
        &journal,
        &server,
    );

    // a credential mapping for a reference no profile uses
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend([
        "--execute-network",
        "--request-cap",
        "4",
        "--credential-env",
        "unused-ref=BENCH_TEST_KEY_A",
    ]);
    assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &extra),
        &services,
        &journal,
        &server,
    );
}

#[test]
fn live_run_with_missing_or_empty_credentials_is_refused_without_naming_them() {
    let authority = TestAuthority::new();
    let server = silent_server(&authority);
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend(["--execute-network", "--request-cap", "4"]);
    let words = live_words(&journal_text, &extra);

    for variables in [
        vec![],
        vec![("BENCH_TEST_KEY_A", SECRET_A)],
        vec![("BENCH_TEST_KEY_A", SECRET_A), ("BENCH_TEST_KEY_B", "")],
    ] {
        let services = fixture_services(&authority, &server, &variables);
        let outcome = assert_refused_before_anything_was_sent(&words, &services, &journal, &server);
        assert!(outcome.err.contains("credential"), "{}", outcome.err);
        assert!(!outcome.err.contains("ref-a") && !outcome.err.contains("ref-b"));
    }

    // an unmapped reference is not silently resolved from the environment
    let unmapped = [
        "--allow-destination",
        "anthropic",
        "--execute-network",
        "--request-cap",
        "4",
    ];
    let services = fixture_services(
        &authority,
        &server,
        &[("ref-a", SECRET_A), ("ref-b", SECRET_B)],
    );
    assert_refused_before_anything_was_sent(
        &live_words(&journal_text, &unmapped),
        &services,
        &journal,
        &server,
    );
}

// ---- live run against a local fixture server ------------------------------------------------

fn tool_use_reply(model: &str) -> Reply {
    let body = json!({
        "id": "msg_synthetic_01",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{
            "type": "tool_use",
            "id": "toolu_synthetic_01",
            "name": "interpret",
            "input": {
                "operation": {"kind": "annotate"},
                "item_type": "action",
                "source_spans": [{"start": 0, "end": 4}]
            }
        }],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 42, "output_tokens": 17}
    });
    Reply::ok(body.to_string().as_bytes())
}

fn request_model(request: &RecordedRequest) -> String {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    body["model"].as_str().unwrap_or_default().to_string()
}

#[test]
fn live_run_against_a_fixture_server_completes_and_never_leaks_secrets() {
    let authority = TestAuthority::new();
    let server = Fixture::start(authority.issue("localhost"), |request| {
        tool_use_reply(&request_model(request))
    });
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = fixture_services(
        &authority,
        &server,
        &[
            ("BENCH_TEST_KEY_A", SECRET_A),
            ("BENCH_TEST_KEY_B", SECRET_B),
        ],
    );
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend([
        "--execute-network",
        "--request-cap",
        "4",
        "--build-revision",
        "fixture-build-3",
    ]);

    let outcome = execute(&live_words(&journal_text, &extra), &services);
    assert_eq!(
        outcome.status, EXIT_COMPLETED,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert!(outcome.out.contains("mode: live"));
    assert!(outcome.out.contains("build revision: fixture-build-3"));
    assert!(outcome
        .out
        .contains("requests reserved this run: 4 of cap 4"));
    assert!(outcome.out.contains("diagnostic only"));
    assert!(!outcome.out.contains("fake mode results"));

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        let model = request_model(request);
        let expected = if model == "model-a" {
            SECRET_A
        } else {
            SECRET_B
        };
        assert!(model == "model-a" || model == "model-b", "{model}");
        assert_eq!(request.header("x-api-key"), Some(expected));
    }
    assert_eq!(
        requests
            .iter()
            .filter(|r| request_model(r) == "model-a")
            .count(),
        2
    );

    let contents = std::fs::read_to_string(&journal).unwrap();
    assert!(contents.contains("mode-live"));
    assert!(contents.contains("fixture-build-3"));
    assert_eq!(attempt_count(&journal), 4);
    for surface in [&outcome.out, &outcome.err, &contents] {
        for forbidden in [
            SECRET_A,
            SECRET_B,
            "BENCH_TEST_KEY",
            "localhost",
            "https://",
            "x-api-key",
            "ref-a",
            "ref-b",
        ] {
            assert!(!surface.contains(forbidden), "leaked {forbidden}");
        }
    }

    // resuming sends nothing
    let resumed = execute(&live_words(&journal_text, &extra), &services);
    assert_eq!(resumed.status, EXIT_COMPLETED, "{}", resumed.err);
    assert_eq!(server.requests().len(), 4);
}

fn chat_completion_reply(model: &str) -> Reply {
    let content = json!({
        "operation": {"kind": "annotate"},
        "item_type": "action",
        "source_spans": [{"start": 0, "end": 4}]
    });
    let body = json!({
        "id": "chatcmpl-synthetic",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content.to_string(), "refusal": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 42, "completion_tokens": 17}
    });
    Reply::ok(body.to_string().as_bytes())
}

#[test]
fn mixed_anthropic_and_openai_live_run_sends_each_credential_only_to_its_own_origin() {
    let authority = TestAuthority::new();
    let anthropic_server = Fixture::start(authority.issue("localhost"), |request| {
        tool_use_reply(&request_model(request))
    });
    let openai_server = Fixture::start(authority.issue("localhost"), |request| {
        chat_completion_reply(&request_model(request))
    });
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let mut services = services_with(&[
        ("BENCH_TEST_KEY_A", SECRET_A),
        ("BENCH_TEST_KEY_B", SECRET_B),
    ]);
    services.anthropic_endpoint = Some(anthropic_server.url("/v1/messages"));
    services.openai_endpoint = Some(openai_server.url("/v1/chat/completions"));
    services.extra_trust_roots = vec![authority.root_der()];
    let words = vec![
        "run",
        "--corpus",
        CORPUS,
        "--profile-a",
        "anthropic:model-a:ref-a",
        "--profile-b",
        "openai:model-b:ref-b",
        "--mode",
        "live",
        "--journal",
        &journal_text,
        "--max-cases",
        "2",
        "--allow-destination",
        "anthropic",
        "--allow-destination",
        "openai",
        "--credential-env",
        "ref-a=BENCH_TEST_KEY_A",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
        "--execute-network",
        "--request-cap",
        "4",
    ];

    let outcome = execute(&words, &services);
    assert_eq!(
        outcome.status, EXIT_COMPLETED,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert!(outcome.out.contains("mode: live"));

    let anthropic_requests = anthropic_server.requests();
    let openai_requests = openai_server.requests();
    assert_eq!(anthropic_requests.len(), 2);
    assert_eq!(openai_requests.len(), 2);
    for request in &anthropic_requests {
        assert_eq!(request_model(request), "model-a");
        assert_eq!(request.header("x-api-key"), Some(SECRET_A));
        assert!(request.header("authorization").is_none());
    }
    let bearer_b = format!("Bearer {SECRET_B}");
    for request in &openai_requests {
        assert_eq!(request_model(request), "model-b");
        assert_eq!(request.header("authorization"), Some(bearer_b.as_str()));
        assert!(request.header("x-api-key").is_none());
    }
    for request in anthropic_requests.iter().chain(&openai_requests) {
        let body = String::from_utf8_lossy(&request.body);
        assert!(!body.contains(SECRET_A) && !body.contains(SECRET_B));
    }
    assert_eq!(attempt_count(&journal), 4);

    let contents = std::fs::read_to_string(&journal).unwrap();
    for surface in [&outcome.out, &outcome.err, &contents] {
        for forbidden in [SECRET_A, SECRET_B, "BENCH_TEST_KEY", "https://", "ref-b"] {
            assert!(!surface.contains(forbidden), "leaked {forbidden}");
        }
    }
}

#[test]
fn openai_arm_without_its_own_destination_grant_is_refused_before_anything_is_sent() {
    let authority = TestAuthority::new();
    let server = silent_server(&authority);
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let mut services = fixture_services(
        &authority,
        &server,
        &[
            ("BENCH_TEST_KEY_A", SECRET_A),
            ("BENCH_TEST_KEY_B", SECRET_B),
        ],
    );
    services.openai_endpoint = Some(server.url("/v1/chat/completions"));
    let words = vec![
        "run",
        "--corpus",
        CORPUS,
        "--profile-a",
        "anthropic:model-a:ref-a",
        "--profile-b",
        "openai:model-b:ref-b",
        "--mode",
        "live",
        "--journal",
        &journal_text,
        "--max-cases",
        "2",
        "--allow-destination",
        "anthropic",
        "--credential-env",
        "ref-a=BENCH_TEST_KEY_A",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
        "--execute-network",
        "--request-cap",
        "4",
    ];
    let outcome = assert_refused_before_anything_was_sent(&words, &services, &journal, &server);
    assert!(outcome.err.contains("--allow-destination openai"));
}

#[test]
fn live_run_with_a_failing_provider_exits_incomplete_without_retrying_beyond_the_allowance() {
    let authority = TestAuthority::new();
    let server = Fixture::start(authority.issue("localhost"), |_| {
        Reply::status(
            401,
            br#"{"type":"error","error":{"type":"authentication_error"}}"#,
        )
    });
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = fixture_services(
        &authority,
        &server,
        &[
            ("BENCH_TEST_KEY_A", SECRET_A),
            ("BENCH_TEST_KEY_B", SECRET_B),
        ],
    );
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend(["--execute-network", "--request-cap", "4"]);

    let outcome = execute(&live_words(&journal_text, &extra), &services);
    assert_eq!(
        outcome.status, EXIT_INCOMPLETE,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert!(server.requests().len() <= 4);
    assert!(journal.exists());
    for surface in [&outcome.out, &outcome.err] {
        assert!(!surface.contains(SECRET_A) && !surface.contains(SECRET_B));
    }
}

// ---- cancellation ---------------------------------------------------------------------------

#[test]
fn a_run_cancelled_before_it_starts_sends_nothing_and_exits_incomplete() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    services.cancel.cancel();
    let outcome = execute(&fake_words(&journal_text, &[]), &services);
    assert_eq!(outcome.status, EXIT_INCOMPLETE, "{}", outcome.err);
    assert!(outcome.out.contains("cancelled: yes"), "{}", outcome.out);
    assert_eq!(attempt_count(&journal), 0);

    // a later uncancelled run resumes the same journal and finishes
    let resumed = execute(&fake_words(&journal_text, &[]), &services_with(&[]));
    assert_eq!(resumed.status, EXIT_COMPLETED, "{}", resumed.err);
    assert_eq!(attempt_count(&journal), 6);
}

#[test]
fn cancelling_while_a_provider_hangs_ends_the_run_promptly_and_records_the_outcome() {
    let authority = TestAuthority::new();
    let server = Fixture::start(authority.issue("localhost"), |_| Reply::Hang);
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = fixture_services(
        &authority,
        &server,
        &[
            ("BENCH_TEST_KEY_A", SECRET_A),
            ("BENCH_TEST_KEY_B", SECRET_B),
        ],
    );
    let token = services.cancel.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(600));
        token.cancel();
    });
    let mut extra: Vec<&str> = GRANTS.to_vec();
    extra.extend([
        "--execute-network",
        "--request-cap",
        "4",
        "--timeout-seconds",
        "120",
    ]);

    let started = Instant::now();
    let outcome = execute(&live_words(&journal_text, &extra), &services);
    canceller.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        outcome.status, EXIT_INCOMPLETE,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert!(outcome.out.contains("cancelled: yes"), "{}", outcome.out);
    assert!(journal.exists());
    assert!(server.requests().len() <= 1);
    for surface in [&outcome.out, &outcome.err] {
        assert!(!surface.contains(SECRET_A) && !surface.contains(SECRET_B));
    }
}
