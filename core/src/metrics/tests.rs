//! Behavior tests for content-free local metrics (T01), run against the real schema.

use super::*;
use crate::store::schema::{Clock, SystemClock};
use serde_json::Value;
use std::sync::Arc;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn memory_database() -> Database {
    Database::open(":memory:", Arc::new(SystemClock) as Arc<dyn Clock>).unwrap()
}

fn report(database: &Database) -> MetricsReport {
    export_metrics(database, ReadAuthentication::Authenticated, now(), None).unwrap()
}

fn timing<'a>(report: &'a MetricsReport, series: &str, dimension: &str) -> &'a TimingSummary {
    report
        .timings
        .iter()
        .find(|summary| summary.series == series && summary.dimension == dimension)
        .unwrap_or_else(|| panic!("no timing for {series}/{dimension}"))
}

fn stored_rows(database: &Database) -> i64 {
    metrics_enabled(database).unwrap();
    let count = |table: &str| -> i64 {
        database
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    count("metrics.metric_timing_samples") + count("metrics.metric_failure_counts")
}

#[test]
fn trigger_to_ready_and_end_of_input_to_save_are_measured_separately() {
    let database = memory_database();
    for milliseconds in [300, 500, 700] {
        record_timing(
            &database,
            TimingSeries::TriggerToReady(LaunchKind::Cold),
            milliseconds,
        )
        .unwrap();
    }
    record_timing(
        &database,
        TimingSeries::TriggerToReady(LaunchKind::Warm),
        90,
    )
    .unwrap();
    record_timing(
        &database,
        TimingSeries::EndOfInputToSave(CaptureKind::Text),
        40,
    )
    .unwrap();
    record_timing(
        &database,
        TimingSeries::EndOfInputToSave(CaptureKind::Voice),
        250,
    )
    .unwrap();

    let report = report(&database);
    let cold = timing(&report, "trigger_to_ready", "cold");
    assert_eq!(
        (
            cold.retained_samples,
            cold.min_ms,
            cold.median_ms,
            cold.max_ms
        ),
        (3, 300, 500, 700)
    );
    assert_eq!(timing(&report, "trigger_to_ready", "warm").median_ms, 90);
    assert_eq!(
        timing(&report, "end_of_input_to_save", "text").median_ms,
        40
    );
    assert_eq!(
        timing(&report, "end_of_input_to_save", "voice").median_ms,
        250
    );
    assert!(
        !report
            .timings
            .iter()
            .any(|summary| summary.series == "trigger_to_ready" && summary.dimension == "text"),
        "a save time never lands in a ready series"
    );
    assert_eq!(report.timings.len(), 4, "series with no data are omitted");
}

#[test]
fn percentiles_use_nearest_rank_over_retained_samples() {
    let database = memory_database();
    let series = TimingSeries::EndOfInputToSave(CaptureKind::Text);
    for milliseconds in (1..=100).rev() {
        record_timing(&database, series, milliseconds).unwrap();
    }
    let report = report(&database);
    let summary = timing(&report, "end_of_input_to_save", "text");
    assert_eq!(summary.retained_samples, 100);
    assert_eq!((summary.min_ms, summary.max_ms), (1, 100));
    assert_eq!((summary.median_ms, summary.p95_ms), (50, 95));

    let single = memory_database();
    record_timing(&single, series, 7).unwrap();
    let report = self::report(&single);
    let summary = timing(&report, "end_of_input_to_save", "text");
    assert_eq!((summary.median_ms, summary.p95_ms), (7, 7));
}

#[test]
fn each_timing_series_keeps_only_its_newest_samples() {
    let database = memory_database();
    let busy = TimingSeries::TriggerToReady(LaunchKind::Cold);
    let quiet = TimingSeries::TriggerToReady(LaunchKind::Warm);
    record_timing(&database, quiet, 11).unwrap();
    let total = MAX_SAMPLES_PER_SERIES as u64 + 100;
    for milliseconds in 1..=total {
        record_timing(&database, busy, milliseconds).unwrap();
    }

    let report = report(&database);
    let cold = timing(&report, "trigger_to_ready", "cold");
    assert_eq!(cold.retained_samples as usize, MAX_SAMPLES_PER_SERIES);
    assert_eq!(cold.min_ms, total - MAX_SAMPLES_PER_SERIES as u64 + 1);
    assert_eq!(cold.max_ms, total);
    assert_eq!(
        timing(&report, "trigger_to_ready", "warm").retained_samples,
        1,
        "a busy series never evicts another's samples"
    );
    assert_eq!(
        report.sample_limit_per_series as usize,
        MAX_SAMPLES_PER_SERIES
    );
}

#[test]
fn failure_counts_are_bounded_by_the_closed_label_set() {
    let database = memory_database();
    for _ in 0..1000 {
        record_failure(
            &database,
            FailureSeries::Processing(ProcessingFailureReason::Timeout),
        )
        .unwrap();
    }
    record_failure(
        &database,
        FailureSeries::Scheduling(SchedulingFailureReason::CapacityExceeded),
    )
    .unwrap();
    assert_eq!(
        stored_rows(&database),
        2,
        "a counter is one row however often it fires"
    );

    let report = report(&database);
    let find = |series: &str, dimension: &str| {
        report
            .failures
            .iter()
            .find(|failure| failure.series == series && failure.dimension == dimension)
            .map(|failure| failure.occurrences)
    };
    assert_eq!(find("processing_failure", "timeout"), Some(1000));
    assert_eq!(find("scheduling_failure", "capacity_exceeded"), Some(1));
    assert_eq!(find("processing_failure", "other"), None);
}

#[test]
fn the_report_makes_no_claim_about_human_attention() {
    let database = memory_database();
    record_failure(
        &database,
        FailureSeries::Processing(ProcessingFailureReason::Unavailable),
    )
    .unwrap();
    record_timing(
        &database,
        TimingSeries::TriggerToReady(LaunchKind::LockedHandoff),
        10,
    )
    .unwrap();
    let json = serde_json::to_value(report(&database)).unwrap();
    let keys = collect_keys(&json);
    for forbidden in [
        "seen",
        "viewed",
        "read",
        "acknowledged",
        "attention",
        "notified",
        "dismissed",
        "handled",
        "resolved",
    ] {
        assert!(
            !keys.iter().any(|key| key.contains(forbidden)),
            "report key mentions {forbidden}: {keys:?}"
        );
    }
    assert!(keys.contains(&"occurrences".to_string()));
}

fn collect_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, inner)| std::iter::once(key.clone()).chain(collect_keys(inner)))
            .collect(),
        Value::Array(values) => values.iter().flat_map(collect_keys).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn the_report_holds_aggregates_and_labels_only() {
    let database = memory_database();
    record_timing(
        &database,
        TimingSeries::EndOfInputToSave(CaptureKind::Voice),
        123,
    )
    .unwrap();
    record_failure(
        &database,
        FailureSeries::Scheduling(SchedulingFailureReason::PermissionDenied),
    )
    .unwrap();
    let json = serde_json::to_value(report(&database)).unwrap();
    let mut keys = collect_keys(&json);
    keys.sort();
    keys.dedup();
    let allowed = [
        "dimension",
        "failures",
        "generated_at",
        "max_ms",
        "median_ms",
        "min_ms",
        "occurrences",
        "p95_ms",
        "recording_enabled",
        "retained_samples",
        "sample_limit_per_series",
        "schema",
        "series",
        "timings",
    ];
    assert_eq!(keys, allowed);
    assert_eq!(json["schema"], REPORT_SCHEMA);
}

#[test]
fn the_stored_tables_have_no_column_that_could_hold_content_or_identifiers() {
    let database = memory_database();
    metrics_enabled(&database).unwrap();
    let columns = |table: &str| -> Vec<String> {
        let mut statement = database
            .conn()
            .prepare("SELECT name FROM pragma_table_info(?1, 'metrics') ORDER BY cid")
            .unwrap();
        statement
            .query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        columns("metric_timing_samples"),
        ["sample_id", "series", "dimension", "duration_ms"]
    );
    assert_eq!(
        columns("metric_failure_counts"),
        ["series", "dimension", "occurrences"]
    );
    assert_eq!(columns("metric_settings"), ["id", "enabled", "updated_at"]);
}

#[test]
fn disabled_metrics_store_nothing_and_stay_disabled_until_enabled_again() {
    let database = memory_database();
    assert!(metrics_enabled(&database).unwrap(), "metrics start on");
    let series = TimingSeries::EndOfInputToSave(CaptureKind::Text);
    let failure = FailureSeries::Processing(ProcessingFailureReason::Timeout);
    assert_eq!(
        record_timing(&database, series, 5).unwrap(),
        RecordOutcome::Recorded
    );

    set_metrics_enabled(&database, false, ReadAuthentication::Authenticated, now()).unwrap();
    assert!(!metrics_enabled(&database).unwrap());
    assert_eq!(
        record_timing(&database, series, 6).unwrap(),
        RecordOutcome::Disabled
    );
    assert_eq!(
        record_failure(&database, failure).unwrap(),
        RecordOutcome::Disabled
    );
    let disabled_report = report(&database);
    assert!(!disabled_report.recording_enabled);
    assert_eq!(
        timing(&disabled_report, "end_of_input_to_save", "text").retained_samples,
        1,
        "disabling keeps what was recorded and records nothing new"
    );
    assert!(disabled_report.failures.is_empty());

    set_metrics_enabled(&database, true, ReadAuthentication::Authenticated, now()).unwrap();
    assert_eq!(
        record_failure(&database, failure).unwrap(),
        RecordOutcome::Recorded
    );
}

#[test]
fn clearing_removes_samples_and_counters_but_not_the_setting() {
    let database = memory_database();
    record_timing(&database, TimingSeries::TriggerToReady(LaunchKind::Cold), 5).unwrap();
    record_failure(
        &database,
        FailureSeries::Processing(ProcessingFailureReason::Other),
    )
    .unwrap();
    set_metrics_enabled(&database, false, ReadAuthentication::Authenticated, now()).unwrap();

    clear_metrics(&database, ReadAuthentication::Authenticated).unwrap();
    assert_eq!(stored_rows(&database), 0);
    assert!(!metrics_enabled(&database).unwrap());
    let cleared = report(&database);
    assert!(cleared.timings.is_empty() && cleared.failures.is_empty());
}

#[test]
fn export_is_refused_without_an_authenticated_session_and_reads_nothing() {
    let database = memory_database();
    record_timing(&database, TimingSeries::TriggerToReady(LaunchKind::Cold), 5).unwrap();
    let refused = export_metrics(&database, ReadAuthentication::CaptureOnly, now(), None);
    assert!(matches!(refused, Err(MetricsError::NotAuthenticated)));
    assert!(export_metrics(&database, ReadAuthentication::Authenticated, now(), None).is_ok());
}

#[test]
fn turning_recording_off_or_on_and_clearing_need_an_authenticated_session() {
    let database = memory_database();
    let series = TimingSeries::TriggerToReady(LaunchKind::Cold);
    record_timing(&database, series, 5).unwrap();

    for enabled in [false, true] {
        let refused =
            set_metrics_enabled(&database, enabled, ReadAuthentication::CaptureOnly, now());
        assert!(matches!(refused, Err(MetricsError::NotAuthenticated)));
    }
    assert!(
        metrics_enabled(&database).unwrap(),
        "a refused switch changes nothing"
    );

    set_metrics_enabled(&database, false, ReadAuthentication::Authenticated, now()).unwrap();
    let refused = set_metrics_enabled(&database, true, ReadAuthentication::CaptureOnly, now());
    assert!(matches!(refused, Err(MetricsError::NotAuthenticated)));
    assert!(
        !metrics_enabled(&database).unwrap(),
        "capture-only cannot resume recording"
    );

    let refused = clear_metrics(&database, ReadAuthentication::CaptureOnly);
    assert!(matches!(refused, Err(MetricsError::NotAuthenticated)));
    assert_eq!(
        stored_rows(&database),
        1,
        "capture-only cannot wipe the evidence"
    );
}

#[test]
fn metrics_never_enter_the_stores_migrated_schema() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("core.sqlite");
    let path = path.to_str().unwrap();
    let database = Database::open(path, Arc::new(SystemClock) as Arc<dyn Clock>).unwrap();
    let version = database.schema_version().unwrap();
    record_timing(&database, TimingSeries::TriggerToReady(LaunchKind::Warm), 7).unwrap();
    let in_main: i64 = database
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM main.sqlite_master WHERE name LIKE 'metric%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(in_main, 0);
    assert_eq!(database.schema_version().unwrap(), version);
    assert!(std::path::Path::new(&format!("{path}.metrics")).exists());
    drop(database);
    // The store validates its shape on open; the sidecar must not disturb that.
    Database::open(path, Arc::new(SystemClock) as Arc<dyn Clock>).unwrap();
}

#[test]
fn recording_never_needs_authentication_and_never_touches_other_tables() {
    let database = memory_database();
    let before: i64 = database
        .conn()
        .query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))
        .unwrap();
    record_timing(
        &database,
        TimingSeries::EndOfInputToSave(CaptureKind::Voice),
        5,
    )
    .unwrap();
    let after: i64 = database
        .conn()
        .query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn implausible_durations_are_refused_not_clamped() {
    let database = memory_database();
    let series = TimingSeries::TriggerToReady(LaunchKind::Warm);
    assert!(matches!(
        record_timing(&database, series, MAX_DURATION_MS + 1),
        Err(MetricsError::DurationOutOfRange)
    ));
    assert!(matches!(
        record_timing(&database, series, u64::MAX),
        Err(MetricsError::DurationOutOfRange)
    ));
    assert_eq!(stored_rows(&database), 0);
    record_timing(&database, series, MAX_DURATION_MS).unwrap();
    record_timing(&database, series, 0).unwrap();
}

#[test]
fn metrics_and_the_enabled_choice_survive_reopening_the_store() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("core.sqlite");
    let path = path.to_str().unwrap();
    {
        let database = Database::open(path, Arc::new(SystemClock) as Arc<dyn Clock>).unwrap();
        record_timing(
            &database,
            TimingSeries::EndOfInputToSave(CaptureKind::Text),
            42,
        )
        .unwrap();
        record_failure(
            &database,
            FailureSeries::Processing(ProcessingFailureReason::Interrupted),
        )
        .unwrap();
        set_metrics_enabled(&database, false, ReadAuthentication::Authenticated, now()).unwrap();
    }
    let database = Database::open(path, Arc::new(SystemClock) as Arc<dyn Clock>).unwrap();
    let reopened = report(&database);
    assert!(!reopened.recording_enabled);
    assert_eq!(
        timing(&reopened, "end_of_input_to_save", "text").median_ms,
        42
    );
    assert_eq!(reopened.failures[0].occurrences, 1);
}

#[test]
fn labels_round_trip_and_unknown_labels_are_rejected() {
    for series in TimingSeries::all() {
        assert_eq!(
            TimingSeries::parse(series.series_label(), series.dimension_label()),
            Some(series)
        );
    }
    for series in FailureSeries::all() {
        assert_eq!(
            FailureSeries::parse(series.series_label(), series.dimension_label()),
            Some(series)
        );
    }
    assert_eq!(TimingSeries::parse("trigger_to_ready", "voice"), None);
    assert_eq!(TimingSeries::parse("anything the user said", "cold"), None);
    assert_eq!(
        FailureSeries::parse("processing_failure", "capacity_exceeded"),
        None
    );
}

#[test]
fn the_module_has_no_network_destination() {
    let source = include_str!("mod.rs");
    for forbidden in [
        "std::net",
        "reqwest",
        "hyper",
        "ureq",
        "TcpStream",
        "UdpSocket",
        "://",
    ] {
        assert!(
            !source.contains(forbidden),
            "metrics must not contain network code or a destination: {forbidden}"
        );
    }
}

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn the_exported_report_names_the_build() {
    let database = memory_database();
    let build = BuildIdentity::new("0.1.0", "412", REVISION, false).unwrap();
    let exported = export_metrics(
        &database,
        ReadAuthentication::Authenticated,
        now(),
        Some(build),
    )
    .unwrap();
    let json = serde_json::to_value(&exported).unwrap();
    assert_eq!(
        json["build"],
        serde_json::json!({
            "version": "0.1.0", "build_number": "412", "revision": REVISION, "dirty": false
        })
    );
    assert_eq!(json["schema"], REPORT_SCHEMA);
    let fields: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for field in fields {
        assert!(
            [
                "schema",
                "generated_at",
                "recording_enabled",
                "sample_limit_per_series",
                "build",
                "timings",
                "failures"
            ]
            .contains(&field),
            "unexpected report field {field}"
        );
    }
}

#[test]
fn a_report_without_a_named_build_has_no_build_field() {
    let database = memory_database();
    let json = serde_json::to_value(report(&database)).unwrap();
    assert!(json.get("build").is_none());
}

#[test]
fn a_build_identity_only_accepts_its_closed_shapes() {
    assert!(BuildIdentity::new("0.1.0-beta.2", "1", REVISION, true).is_ok());
    for (version, build_number, revision) in [
        ("", "1", REVISION),
        ("0.1.0 draft", "1", REVISION),
        ("a note the user wrote", "1", REVISION),
        ("0.1.0", "", REVISION),
        ("0.1.0", "12345678901", REVISION),
        ("0.1.0", "1.2", REVISION),
        ("0.1.0", "1", "unknown"),
        ("0.1.0", "1", "0123456"),
        ("0.1.0", "1", &REVISION.to_uppercase()),
    ] {
        assert!(
            matches!(
                BuildIdentity::new(version, build_number, revision, false),
                Err(MetricsError::InvalidBuildIdentity)
            ),
            "{version}/{build_number}/{revision} must be refused"
        );
    }
}
