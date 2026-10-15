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
        // a self-hosted profile needs its endpoint variable
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
fn plan_with_a_different_model_than_the_journal_refuses_with_the_configuration_change() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &[]), &services).status,
        EXIT_COMPLETED
    );
    let before = std::fs::read(&journal).unwrap();
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
    assert_eq!(outcome.status, EXIT_REFUSED, "{}", outcome.out);
    assert!(outcome.out.is_empty(), "{}", outcome.out);
    assert!(
        outcome
            .err
            .contains("experiment configuration changed (profile_a_id, profile_a_version)"),
        "{}",
        outcome.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);
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

fn plan_words<'a>(journal: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = fake_words(journal, extra);
    words[0] = "plan";
    words
}

#[test]
fn plan_refuses_a_rerun_on_a_fresh_journal_exactly_as_run_does() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let target = format!("{}:a", first_case_id());
    let extra = ["--max-cases", "1", "--rerun", &target];

    let planned = execute(&plan_words(&journal_text, &extra), &services);
    assert_eq!(planned.status, EXIT_REFUSED, "{}", planned.out);
    assert!(
        planned.err.contains("no dispatched attempt"),
        "{}",
        planned.err
    );
    assert!(!planned.out.contains("planned requests"), "{}", planned.out);
    assert!(!journal.exists(), "plan must not create the journal");

    let ran = execute(&fake_words(&journal_text, &extra), &services);
    assert_eq!(ran.status, EXIT_REFUSED, "{}", ran.err);
    assert!(ran.err.contains("no dispatched attempt"), "{}", ran.err);
    assert!(!journal.exists());
}

#[test]
fn plan_with_a_fresh_journal_succeeds_without_creating_it() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);

    let planned = execute(&plan_words(&journal_text, &["--max-cases", "2"]), &services);
    assert_eq!(planned.status, EXIT_COMPLETED, "{}", planned.err);
    assert!(
        planned.out.contains("planned requests: 4"),
        "{}",
        planned.out
    );
    assert!(planned.err.is_empty());
    assert!(
        !journal.exists(),
        "plan must not create the requested journal"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "plan must write nothing beside the requested journal"
    );
}

#[test]
fn plan_refuses_unplanned_and_undispatched_reruns_over_an_existing_journal() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal_text, &["--max-cases", "2"]), &services).status,
        EXIT_COMPLETED
    );
    let before = std::fs::read(&journal).unwrap();

    let unplanned = execute(
        &plan_words(
            &journal_text,
            &["--max-cases", "2", "--rerun", "no-such-case:a"],
        ),
        &services,
    );
    assert_eq!(unplanned.status, EXIT_REFUSED, "{}", unplanned.out);
    assert!(
        unplanned.err.contains("not in the plan"),
        "{}",
        unplanned.err
    );

    // Planned by --max-cases 3 but never dispatched in the two-case run.
    let undispatched_target = format!("{}:b", nth_case_id(2));
    let undispatched = execute(
        &plan_words(&journal_text, &["--rerun", &undispatched_target]),
        &services,
    );
    assert_eq!(undispatched.status, EXIT_REFUSED, "{}", undispatched.out);
    assert!(
        undispatched.err.contains("no dispatched attempt"),
        "{}",
        undispatched.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);

    // A dispatched side is accepted by plan, and run agrees.
    let dispatched_target = format!("{}:a", first_case_id());
    let accepted = execute(
        &plan_words(&journal_text, &["--rerun", &dispatched_target]),
        &services,
    );
    assert_eq!(accepted.status, EXIT_COMPLETED, "{}", accepted.err);
    assert!(
        accepted.out.contains("planned requests"),
        "{}",
        accepted.out
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    let ran = execute(
        &fake_words(&journal_text, &["--rerun", &dispatched_target]),
        &services,
    );
    assert_eq!(ran.status, EXIT_COMPLETED, "{}", ran.err);
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

/// Drops the journal's final newline, leaving a torn tail that opening it for writing repairs.
fn tear_tail(path: &Path) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    std::fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn changed_configuration_leaves_a_torn_journal_byte_identical_and_resume_still_repairs_it() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let first = ["--max-cases", "1", "--build-revision", "build-1"];
    assert_eq!(
        execute(&fake_words(&journal_text, &first), &services).status,
        EXIT_COMPLETED
    );
    let torn = tear_tail(&journal);

    let changed = execute(
        &fake_words(
            &journal_text,
            &["--max-cases", "1", "--build-revision", "build-2"],
        ),
        &services,
    );
    assert_eq!(changed.status, EXIT_REFUSED, "{}", changed.err);
    assert!(changed.err.contains("build_revision"), "{}", changed.err);
    assert_eq!(std::fs::read(&journal).unwrap(), torn);

    let mut words = fake_words(&journal_text, &first);
    let position = words
        .iter()
        .position(|word| *word == "openai:model-b")
        .unwrap();
    words[position] = "openai:model-other";
    let changed_model = execute(&words, &services);
    assert_eq!(changed_model.status, EXIT_REFUSED, "{}", changed_model.err);
    assert_eq!(std::fs::read(&journal).unwrap(), torn);

    let resumed = execute(&fake_words(&journal_text, &first), &services);
    assert_eq!(resumed.status, EXIT_COMPLETED, "{}", resumed.err);
    assert_eq!(std::fs::read(&journal).unwrap().last(), Some(&b'\n'));
    assert_eq!(attempt_count(&journal), 2);
}

#[test]
fn a_journaled_case_that_differs_from_the_corpus_is_refused_before_the_journal_is_opened() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let words = fake_words(&journal_text, &["--max-cases", "1"]);
    assert_eq!(execute(&words, &services).status, EXIT_COMPLETED);
    let original = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(original.matches("roof garden").count(), 1, "{original}");
    std::fs::write(&journal, original.replace("roof garden", "roof terrace")).unwrap();
    let torn = tear_tail(&journal);

    let refused = execute(&words, &services);
    assert_eq!(refused.status, EXIT_REFUSED, "{}", refused.err);
    assert!(
        refused.err.contains("changed since it was journaled"),
        "{}",
        refused.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
}

#[test]
fn a_journal_with_records_but_no_experiment_is_refused_byte_identical() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let words = fake_words(&journal_text, &["--max-cases", "1"]);
    assert_eq!(execute(&words, &services).status, EXIT_COMPLETED);
    let original = std::fs::read_to_string(&journal).unwrap();
    let without_experiment: String = original
        .lines()
        .skip(1)
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&journal, &without_experiment).unwrap();
    assert!(JournalReader::open(&journal)
        .unwrap()
        .experiments()
        .is_empty());
    let torn = tear_tail(&journal);

    let refused = execute(&words, &services);
    assert_eq!(refused.status, EXIT_REFUSED, "{}", refused.err);
    assert!(refused.err.contains("use a new journal"), "{}", refused.err);
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
}

/// Runs a one-case fake journal, rewrites its records with `tamper`, tears its tail, and returns
/// the refusal of `run` after checking that `plan` refuses the same way and that neither
/// changed a byte of the journal.
fn refusal_of_a_tampered_journal(tamper: impl FnOnce(Vec<Value>) -> Vec<Value>) -> String {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let services = services_with(&[]);
    let extra = ["--max-cases", "1"];
    assert_eq!(
        execute(&fake_words(&journal_text, &extra), &services).status,
        EXIT_COMPLETED
    );
    let records: Vec<Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rewritten: String = tamper(records)
        .iter()
        .map(|record| format!("{record}\n"))
        .collect();
    std::fs::write(&journal, rewritten).unwrap();
    let torn = tear_tail(&journal);

    let planned = execute(&plan_words(&journal_text, &extra), &services);
    assert_eq!(planned.status, EXIT_REFUSED, "{}", planned.out);
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
    let refused = execute(&fake_words(&journal_text, &extra), &services);
    assert_eq!(refused.status, EXIT_REFUSED, "{}", refused.out);
    assert!(refused.err.contains("nothing was sent"), "{}", refused.err);
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
    assert_eq!(planned.err, refused.err);
    refused.err
}

fn is_case(record: &Value) -> bool {
    record["record_type"] == "case"
}

fn first_attempt(records: &mut [Value]) -> &mut Value {
    records
        .iter_mut()
        .find(|record| record["record_type"] == "attempt")
        .unwrap()
}

#[test]
fn plan_and_run_agree_on_refusing_a_changed_journaled_case_text() {
    let refusal = refusal_of_a_tampered_journal(|mut records| {
        let case = records.iter_mut().find(|record| is_case(record)).unwrap();
        let text = case["content"]
            .as_str()
            .unwrap()
            .replace("roof garden", "roof terrace");
        case["content"] = json!(text);
        records
    });
    let expected = format!("case {} changed since it was journaled", first_case_id());
    assert!(refusal.contains(&expected), "{refusal}");
}

#[test]
fn a_journal_recording_a_case_twice_is_refused_byte_identical() {
    let refusal = refusal_of_a_tampered_journal(|mut records| {
        let case = records
            .iter()
            .find(|record| is_case(record))
            .unwrap()
            .clone();
        records.push(case);
        records
    });
    let expected = format!(
        "the journal records case {} more than once",
        first_case_id()
    );
    assert!(refusal.contains(&expected), "{refusal}");
}

#[test]
fn a_journal_case_of_another_experiment_is_refused_byte_identical() {
    let refusal = refusal_of_a_tampered_journal(|mut records| {
        let case = records.iter_mut().find(|record| is_case(record)).unwrap();
        case["experiment_id"] = json!("00000000-0000-4000-8000-000000000000");
        records
    });
    assert!(
        refusal.contains("belongs to another experiment"),
        "{refusal}"
    );
}

#[test]
fn a_journal_attempt_without_its_case_is_refused_byte_identical() {
    let refusal = refusal_of_a_tampered_journal(|records| {
        records
            .into_iter()
            .filter(|record| !is_case(record))
            .collect()
    });
    assert!(
        refusal.contains("an attempt for a case it does not record"),
        "{refusal}"
    );
}

#[test]
fn a_journal_attempt_of_another_executor_version_is_refused_byte_identical() {
    let refusal = refusal_of_a_tampered_journal(|mut records| {
        first_attempt(&mut records)["provider_output"]["executor_record"] = json!(2);
        records
    });
    assert!(
        refusal.contains("an attempt this executor did not write"),
        "{refusal}"
    );
}

#[test]
fn a_journal_attempt_for_an_unknown_arm_is_refused_byte_identical() {
    refusal_of_a_tampered_journal(|mut records| {
        first_attempt(&mut records)["arm"] = json!("c");
        records
    });
}

#[test]
fn executor_refusals_the_read_only_checks_do_not_name_are_still_made_before_the_journal_is_opened()
{
    // An envelope of the supported version that the executor cannot read.
    let unreadable = refusal_of_a_tampered_journal(|mut records| {
        first_attempt(&mut records)["provider_output"]["phase"] = json!("unheard-of");
        records
    });
    assert!(
        unreadable.contains("records this executor did not write"),
        "{unreadable}"
    );
    // A derived request that no longer matches the one journaled for the case. A later record of
    // an attempt supersedes its reservation, so every record is changed.
    let changed_request = refusal_of_a_tampered_journal(|mut records| {
        for record in &mut records {
            if record["record_type"] == "attempt" {
                record["provider_output"]["pair_fingerprint"] = json!("0".repeat(64));
            }
        }
        records
    });
    assert!(
        changed_request.contains("configuration changed: Request"),
        "{changed_request}"
    );
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

// ---- self-hosted profiles ---------------------------------------------------------------------

const ENDPOINT_VARIABLE: &str = "BENCH_TEST_SPARK_URL";
const SPARK_BASE: &str = "https://spark.tailnet.example:8443/v1";
const VERIFIED_MODEL_SPEC: &str = "deepseek-flash-iq3@latest";
const UNVERIFIED_MODEL_SPEC: &str = "qwen3.8@27b";

fn self_hosted_words<'a>(command: &'a str, model_spec: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = vec![
        command,
        "--corpus",
        CORPUS,
        "--profile-a",
        model_spec,
        "--profile-b",
        "anthropic:model-b:ref-b",
        "--self-hosted-endpoint-env",
        ENDPOINT_VARIABLE,
        "--max-cases",
        "2",
    ];
    words.extend_from_slice(extra);
    words
}

fn spec(model: &str) -> String {
    format!("self-hosted:{model}:ref-spark")
}

fn plan_text(model_spec: &str, base_url: &str, extra: &[&str]) -> Outcome {
    let spec = spec(model_spec);
    let words = self_hosted_words("plan", &spec, extra);
    execute(&words, &services_with(&[(ENDPOINT_VARIABLE, base_url)]))
}

#[test]
fn self_hosted_profile_arguments_are_validated_without_echoing_the_endpoint() {
    let spec = spec(VERIFIED_MODEL_SPEC);
    let with_endpoint_variable = |value: &'static str| {
        let words = self_hosted_words("plan", &spec, &[]);
        execute(&words, &services_with(&[(ENDPOINT_VARIABLE, value)]))
    };

    // a model tag is written with `@`; a literal `:` would be the credential separator
    let outcome = plan_text(VERIFIED_MODEL_SPEC, SPARK_BASE, &[]);
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(outcome
        .out
        .contains("self-hosted / deepseek-flash-iq3:latest"));
    assert!(!outcome.out.contains('@'));

    // `@` is only a self-hosted model tag
    let hosted_tag = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "openai:model@tag",
            "--profile-b",
            "anthropic:model-b",
        ],
        &services_with(&[]),
    );
    assert_eq!(hosted_tag.status, EXIT_REFUSED);

    // the variable is required, must be named rather than given, and must be set
    let words = self_hosted_words("plan", &spec, &[]);
    let without_variable: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| *word != "--self-hosted-endpoint-env" && *word != ENDPOINT_VARIABLE)
        .collect();
    let outcome = execute(&without_variable, &services_with(&[]));
    assert_eq!(outcome.status, EXIT_REFUSED);
    assert!(outcome.err.contains("--self-hosted-endpoint-env"));

    let by_value = execute(
        &self_hosted_words("plan", &spec, &[])
            .iter()
            .map(|word| {
                if *word == ENDPOINT_VARIABLE {
                    SPARK_BASE
                } else {
                    word
                }
            })
            .collect::<Vec<_>>(),
        &services_with(&[]),
    );
    assert_eq!(by_value.status, EXIT_REFUSED);
    assert!(!by_value.err.contains("tailnet"), "{}", by_value.err);

    let unset = execute(&self_hosted_words("plan", &spec, &[]), &services_with(&[]));
    assert_eq!(unset.status, EXIT_REFUSED);
    let empty = with_endpoint_variable("");
    assert_eq!(empty.status, EXIT_REFUSED);

    for unsafe_value in [
        "http://spark.tailnet.example/v1",
        "https://user:pw-canary@spark.tailnet.example/v1",
        "https://spark.tailnet.example/v1?token=canary",
        "spark.tailnet.example",
    ] {
        let outcome = with_endpoint_variable(unsafe_value);
        assert_eq!(outcome.status, EXIT_REFUSED, "{unsafe_value}");
        assert!(outcome.out.is_empty());
        for surface in [&outcome.out, &outcome.err] {
            assert!(
                !surface.contains("tailnet") && !surface.contains("canary"),
                "{surface}"
            );
        }
    }

    // naming an endpoint variable without a self-hosted profile is refused, not ignored
    let outcome = execute(
        &[
            "plan",
            "--corpus",
            CORPUS,
            "--profile-a",
            "openai:model-a",
            "--profile-b",
            "anthropic:model-b",
            "--self-hosted-endpoint-env",
            ENDPOINT_VARIABLE,
        ],
        &services_with(&[(ENDPOINT_VARIABLE, SPARK_BASE)]),
    );
    assert_eq!(outcome.status, EXIT_REFUSED);
}

#[test]
fn the_plan_pins_model_and_endpoint_into_the_profile_version_and_never_prints_the_endpoint() {
    let version_line = |outcome: &Outcome| -> String {
        outcome
            .out
            .lines()
            .find(|line| line.starts_with("profile a:"))
            .unwrap_or_else(|| panic!("no profile line in {}", outcome.out))
            .to_string()
    };
    let base = plan_text(VERIFIED_MODEL_SPEC, SPARK_BASE, &[]);
    let other_endpoint = plan_text(
        VERIFIED_MODEL_SPEC,
        "https://other-spark.tailnet.example:8443/v1",
        &[],
    );
    let other_path = plan_text(
        VERIFIED_MODEL_SPEC,
        "https://spark.tailnet.example:8443/v2",
        &[],
    );
    let other_model = plan_text(UNVERIFIED_MODEL_SPEC, SPARK_BASE, &[]);
    for outcome in [&base, &other_endpoint, &other_path, &other_model] {
        assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
        assert!(!outcome.out.contains("tailnet") && !outcome.out.contains("8443"));
    }
    assert!(version_line(&base).contains("deepseek-flash-iq3:latest"));
    assert!(version_line(&other_model).contains("qwen3.8:27b"));
    let versions = [&base, &other_endpoint, &other_path, &other_model].map(|outcome| {
        version_line(outcome)
            .rsplit("version ")
            .next()
            .unwrap()
            .to_string()
    });
    for (index, version) in versions.iter().enumerate() {
        assert!(
            !versions[..index].contains(version),
            "every configured difference is a different profile version"
        );
    }
    // the same configuration gives the same version, so a run can be resumed
    assert_eq!(
        version_line(&base),
        version_line(&plan_text(VERIFIED_MODEL_SPEC, SPARK_BASE, &[]))
    );
}

#[test]
fn the_plan_names_why_an_unverified_model_cannot_be_paired_and_plans_no_request() {
    let outcome = plan_text(UNVERIFIED_MODEL_SPEC, SPARK_BASE, &[]);
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    assert!(
        outcome
            .out
            .contains("not eligible, profile a: text interpretation is not verified"),
        "{}",
        outcome.out
    );
    assert!(
        outcome.out.contains("planned requests: 0"),
        "{}",
        outcome.out
    );
    assert!(outcome.out.contains("sends nothing, for either arm"));
    assert!(!outcome.out.contains("to send"), "{}", outcome.out);
    assert!(!outcome.out.contains("not eligible, profile b"));

    let unlisted = plan_text("not-a-listed-model", SPARK_BASE, &[]);
    assert!(
        unlisted.out.contains("not eligible, profile a"),
        "{}",
        unlisted.out
    );
}

#[test]
fn a_verified_model_is_paired_only_as_far_as_the_adapter_declares_diagnostic_support() {
    use crate::adapters::self_hosted::diagnostic_support;
    let outcome = plan_text(VERIFIED_MODEL_SPEC, SPARK_BASE, &[]);
    assert_eq!(outcome.status, EXIT_COMPLETED, "{}", outcome.err);
    // the verified model is not blocked on evidence; any block is the adapter's own declaration
    assert!(
        !outcome.out.contains("is not verified for this model"),
        "{}",
        outcome.out
    );
    if diagnostic_support().is_supported() {
        assert!(
            !outcome.out.contains("not eligible, profile a"),
            "{}",
            outcome.out
        );
    } else {
        assert!(
            outcome.out.contains(
                "not eligible, profile a: the adapter declares no support for diagnostic calls"
            ),
            "{}",
            outcome.out
        );
        assert!(outcome.out.contains("planned requests: 0"));
    }
}

#[test]
fn fake_run_with_an_unverified_self_hosted_arm_skips_every_case_and_sends_nothing() {
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let spec = spec(UNVERIFIED_MODEL_SPEC);
    let words = self_hosted_words("run", &spec, &["--journal", &journal_text]);
    let services = services_with(&[(ENDPOINT_VARIABLE, SPARK_BASE)]);

    let outcome = execute(&words, &services);
    assert_eq!(
        outcome.status, EXIT_INCOMPLETE,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert_eq!(attempt_count(&journal), 0);
    let journal_text_content = std::fs::read_to_string(&journal).unwrap();
    assert!(
        journal_text_content.contains("not_eligible"),
        "{journal_text_content}"
    );
    assert!(
        journal_text_content.contains("unverified"),
        "{journal_text_content}"
    );
    // The journal and the report carry random UUIDs and SHA-256 hex digests, so a bare "8443" shows
    // up in them by chance. The port can only leak as part of the endpoint, written ":8443".
    for surface in [&outcome.out, &outcome.err, &journal_text_content] {
        assert!(!surface.contains("tailnet") && !surface.contains(":8443"));
    }

    // another endpoint is another profile: the pinned journal is not silently continued
    let moved = services_with(&[(
        ENDPOINT_VARIABLE,
        "https://other-spark.tailnet.example:8443/v1",
    )]);
    let before = std::fs::read(&journal).unwrap();
    let refused = execute(&words, &moved);
    assert_eq!(refused.status, EXIT_REFUSED, "{}", refused.out);
    assert!(
        refused.err.contains("configuration changed"),
        "{}",
        refused.err
    );
    assert_eq!(std::fs::read(&journal).unwrap(), before);
}

fn live_self_hosted_words<'a>(journal: &'a str, spec: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = self_hosted_words("run", spec, &["--journal", journal, "--mode", "live"]);
    words.extend_from_slice(extra);
    words
}

#[test]
fn live_self_hosted_run_needs_its_own_grant_and_credential_and_never_falls_back() {
    let authority = TestAuthority::new();
    let cloud = silent_server(&authority);
    let spark = silent_server(&authority);
    let dir = TempDir::new().unwrap();
    let (journal, journal_text) = journal_in(&dir);
    let spec = spec(UNVERIFIED_MODEL_SPEC);
    let spark_url = spark.url("/v1");
    let both = [
        (ENDPOINT_VARIABLE, spark_url.as_str()),
        ("BENCH_TEST_KEY_A", SECRET_A),
        ("BENCH_TEST_KEY_B", SECRET_B),
    ];
    let services = |variables: &[(&str, &str)]| {
        let mut services = fixture_services(&authority, &cloud, variables);
        services.extra_trust_roots = vec![authority.root_der()];
        services
    };
    let granted = [
        "--execute-network",
        "--request-cap",
        "4",
        "--allow-destination",
        "self-hosted",
        "--allow-destination",
        "anthropic",
        "--credential-env",
        "ref-spark=BENCH_TEST_KEY_A",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
    ];
    let refuse = |extra: &[&str], variables: &[(&str, &str)]| {
        let outcome = execute(
            &live_self_hosted_words(&journal_text, &spec, extra),
            &services(variables),
        );
        assert_eq!(outcome.status, EXIT_REFUSED, "{}", outcome.err);
        assert!(outcome.out.is_empty());
        assert!(outcome.err.contains("nothing was sent"));
        assert!(!journal.exists(), "the journal is not opened on a refusal");
        assert_eq!(spark.connections() + cloud.connections(), 0);
        for surface in [&outcome.out, &outcome.err] {
            assert!(!surface.contains(SECRET_A) && !surface.contains(SECRET_B));
            assert!(!surface.contains(&spark_url));
        }
        outcome
    };

    // choosing a self-hosted profile grants nothing
    let without_grant = [
        "--execute-network",
        "--request-cap",
        "4",
        "--allow-destination",
        "anthropic",
        "--credential-env",
        "ref-spark=BENCH_TEST_KEY_A",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
    ];
    let outcome = refuse(&without_grant, &both);
    assert!(
        outcome.err.contains("--allow-destination self-hosted"),
        "{}",
        outcome.err
    );

    // a missing or empty credential is refused, never replaced by the other arm's
    let outcome = refuse(&granted, &[both[0], both[2]]);
    assert!(outcome.err.contains("credential"), "{}", outcome.err);
    refuse(&granted, &[both[0], both[2], ("BENCH_TEST_KEY_A", "")]);

    // an unmapped reference is not resolved from the environment
    let unmapped = [
        "--execute-network",
        "--request-cap",
        "4",
        "--allow-destination",
        "self-hosted",
        "--allow-destination",
        "anthropic",
        "--credential-env",
        "ref-b=BENCH_TEST_KEY_B",
    ];
    refuse(&unmapped, &both);

    // with everything granted, an arm that cannot be paired still sends nothing, on either side
    let outcome = execute(
        &live_self_hosted_words(&journal_text, &spec, &granted),
        &services(&both),
    );
    assert_eq!(
        outcome.status, EXIT_INCOMPLETE,
        "{}\n{}",
        outcome.out, outcome.err
    );
    assert_eq!(spark.connections(), 0);
    assert_eq!(cloud.connections(), 0, "the hosted arm is not run alone");
    assert_eq!(attempt_count(&journal), 0);
    let recorded = std::fs::read_to_string(&journal).unwrap();
    for surface in [&outcome.out, &outcome.err, &recorded] {
        assert!(!surface.contains(SECRET_A) && !surface.contains(SECRET_B));
        assert!(!surface.contains(&spark_url) && !surface.contains("localhost"));
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

// ---- report ---------------------------------------------------------------------------------

#[test]
fn report_renders_a_fake_run_as_text_and_json_without_touching_the_journal() {
    let dir = TempDir::new().unwrap();
    let (path, journal) = journal_in(&dir);
    let services = services_with(&[]);
    assert_eq!(
        execute(&fake_words(&journal, &[]), &services).status,
        EXIT_COMPLETED
    );
    let before = std::fs::read(&path).unwrap();

    let text = execute(
        &["report", "--corpus", CORPUS, "--journal", &journal],
        &services,
    );
    assert_eq!(text.status, EXIT_COMPLETED, "{}", text.err);
    assert!(text.out.starts_with("bench report (diagnostic only)"));
    assert!(text.out.contains("matched 3  failed 0  missing 0"));
    assert!(text.out.contains("[fake_execution]"));
    assert!(text.out.contains("cost: unknown (no_rates_supplied)"));
    assert!(text.err.is_empty());

    let json = execute(
        &[
            "report",
            "--corpus",
            CORPUS,
            "--journal",
            &journal,
            "--format",
            "json",
        ],
        &services,
    );
    assert_eq!(json.status, EXIT_COMPLETED, "{}", json.err);
    let parsed: Value = serde_json::from_str(&json.out).unwrap();
    assert_eq!(parsed["pairs"]["matched"], json!(3));
    assert_eq!(parsed["selection"]["winner"], Value::Null);
    assert_eq!(parsed["experiment"]["execution"], json!("fake"));

    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn report_costs_only_from_a_supplied_rates_file_and_never_invents_usage() {
    let dir = TempDir::new().unwrap();
    let (_, journal) = journal_in(&dir);
    let services = services_with(&[]);
    execute(&fake_words(&journal, &[]), &services);
    let base = ["report", "--corpus", CORPUS, "--journal", &journal];

    let mut json_words = base.to_vec();
    json_words.extend(["--format", "json"]);
    let unpriced: Value = serde_json::from_str(&execute(&json_words, &services).out).unwrap();
    assert_eq!(
        unpriced["operations"][0]["cost"]["reason"],
        json!("no_rates_supplied")
    );
    let version = unpriced["experiment"]["profiles"][0]["profile_version"]
        .as_str()
        .unwrap()
        .to_string();

    let write_rates = |profiles: Value| {
        let path = dir.path().join("rates.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "rates_version": "rates-test",
                "currency": "USD",
                "profiles": profiles,
            }))
            .unwrap(),
        )
        .unwrap();
        path.to_str().unwrap().to_string()
    };

    let rates = write_rates(json!([{
        "profile_version": version,
        "input_micros_per_million_tokens": 1,
        "output_micros_per_million_tokens": 1,
    }]));
    let mut priced_words = json_words.clone();
    priced_words.extend(["--rates", &rates]);
    let priced = execute(&priced_words, &services);
    assert_eq!(priced.status, EXIT_COMPLETED, "{}", priced.err);
    let priced: Value = serde_json::from_str(&priced.out).unwrap();
    // Fake replies report no usage, so even a supplied rate yields an unknown cost, not zero.
    assert_eq!(priced["operations"][0]["cost"]["status"], json!("unknown"));
    assert_eq!(
        priced["operations"][0]["cost"]["reason"],
        json!("no_usage_reported")
    );
    assert_eq!(
        priced["operations"][1]["cost"]["reason"],
        json!("no_rate_for_profile_version")
    );

    let invalid = write_rates(json!([]));
    let mut invalid_words = base.to_vec();
    invalid_words.extend(["--rates", &invalid]);
    let outcome = execute(&invalid_words, &services);
    assert_eq!(outcome.status, EXIT_REFUSED);
    assert!(outcome.out.is_empty());
    assert!(outcome.err.starts_with("bench: "));
}

#[test]
fn report_refusals_exit_two_and_send_or_write_nothing() {
    let dir = TempDir::new().unwrap();
    let (path, journal) = journal_in(&dir);
    let services = services_with(&[]);

    for words in [
        vec!["report", "--corpus", CORPUS],
        vec!["report", "--journal", &journal],
        vec![
            "report",
            "--corpus",
            CORPUS,
            "--journal",
            &journal,
            "--format",
            "html",
        ],
        vec![
            "report",
            "--corpus",
            CORPUS,
            "--journal",
            &journal,
            "--surprise",
        ],
        vec!["report", "--corpus", CORPUS, "--journal", &journal],
    ] {
        let outcome = execute(&words, &services);
        assert_eq!(outcome.status, EXIT_REFUSED, "{words:?}");
        assert!(outcome.out.is_empty(), "{words:?}");
        assert!(!outcome.err.is_empty(), "{words:?}");
    }
    assert!(!path.exists(), "reporting must never create a journal");
}

#[test]
fn usage_lists_the_report_command() {
    assert!(USAGE.contains("bench report"));
    assert!(USAGE.contains("--rates"));
    let help = execute(&["report", "--help"], &services_with(&[]));
    assert_eq!(help.status, EXIT_COMPLETED);
}
