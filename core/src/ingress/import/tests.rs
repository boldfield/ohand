use super::*;
use crate::domain::items::{load_item_state, verify_state_integrity, LifecycleState};
use crate::retrieval::index::search_index;
use crate::store::captures::save_capture;
use crate::store::schema::SystemClock;
use std::sync::Arc;
use tempfile::TempDir;

const CAPTURE_ID: &str = "3f9c1e52-0000-4000-8000-0000000000a1";
const ROUTE_ID: &str = "route-personal";

struct Store {
    _directory: TempDir,
    path: String,
}

impl Store {
    fn new() -> Store {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("ohand.db").display().to_string();
        let store = Store {
            _directory: directory,
            path,
        };
        let database = store.open();
        for (route_id, scope) in [(ROUTE_ID, "personal"), ("route-work", "work")] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-08T09:00:00Z')",
                    rusqlite::params![route_id, route_id, scope],
                )
                .unwrap();
        }
        store
    }

    /// Opening again stands in for a restart.
    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn text_capture(text: &str) -> Capture {
    Capture::new(
        CAPTURE_ID.to_string(),
        Some(text.to_string()),
        None,
        "2026-10-08T09:30:00Z".to_string(),
        "America/New_York".to_string(),
        -240,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        ROUTE_ID.to_string(),
        false,
        "2026-10-08T09:30:01Z".to_string(),
        Some("garden".to_string()),
    )
    .unwrap()
}

fn audio_capture() -> Capture {
    Capture {
        text: None,
        audio_reference: Some("staging/2026-10-08/clip-1.m4a".to_string()),
        ..text_capture("unused")
    }
}

fn validation(error: &IngressError) -> &IngressRejection {
    match &error.kind {
        IngressErrorKind::Validation(rejection) => rejection,
        other => panic!("expected a validation rejection, got {other:?}"),
    }
}

fn assert_nothing_stored(database: &Database) {
    for table in ["captures", "items", "search_index"] {
        assert_eq!(count(database, table), 0, "{table} must be empty");
    }
}

#[test]
fn text_import_creates_item_projection_and_index_row_together() {
    let store = Store::new();
    let mut database = store.open();

    let acknowledgment =
        import_foreground_ingress(&mut database, &text_capture("buy oat milk")).unwrap();
    assert_eq!(acknowledgment.capture_id, CAPTURE_ID);
    assert_eq!(acknowledgment.saved_at, "2026-10-08T09:30:01Z");
    assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);

    let (save, sync, processing, transcription, lifecycle, revision): (
        String,
        String,
        String,
        String,
        String,
        i32,
    ) = database
        .conn()
        .query_row(
            "SELECT save_state, sync_state, processing_state, transcription_state,
                    lifecycle_state, revision FROM items WHERE item_id = ?",
            [&acknowledgment.item_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        (save.as_str(), sync.as_str(), processing.as_str()),
        ("saved_local", "not_configured", "unprocessed")
    );
    assert_eq!(
        (transcription.as_str(), lifecycle.as_str(), revision),
        ("not_applicable", "active", 0)
    );

    let transaction = database.transaction().unwrap();
    let state = load_item_state(&transaction, &acknowledgment.item_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.capture_id, CAPTURE_ID);
    assert_eq!(state.lifecycle_state, LifecycleState::Active);
    assert_eq!(state.session_topic.as_deref(), Some("garden"));
    assert!(
        verify_state_integrity(&transaction, &acknowledgment.item_id)
            .unwrap()
            .is_none()
    );
    drop(transaction);

    let hits = search_index(database.conn(), "oat", &[ItemScope::Personal]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item_id, acknowledgment.item_id);
    assert_eq!(hits[0].current_text, "buy oat milk");
}

#[test]
fn audio_import_awaits_transcription_and_has_no_text_to_index() {
    let store = Store::new();
    let mut database = store.open();

    let acknowledgment = import_foreground_ingress(&mut database, &audio_capture()).unwrap();

    let transcription: String = database
        .conn()
        .query_row(
            "SELECT transcription_state FROM items WHERE item_id = ?",
            [&acknowledgment.item_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(transcription, "audio_pending");
    assert_eq!(count(&database, "search_index"), 0);
}

#[test]
fn identical_retries_converge_across_restarts() {
    let store = Store::new();
    let capture = text_capture("call the dentist");

    let first = import_foreground_ingress(&mut store.open(), &capture).unwrap();
    let second = import_foreground_ingress(&mut store.open(), &capture).unwrap();
    let third = import_foreground_ingress(&mut store.open(), &capture).unwrap();

    assert_eq!(second.item_id, first.item_id);
    assert_eq!(third.item_id, first.item_id);
    assert_eq!(second.saved_at, first.saved_at);
    assert_eq!(second.disposition, ImportDisposition::AlreadyImported);
    let database = store.open();
    assert_eq!(count(&database, "captures"), 1);
    assert_eq!(count(&database, "items"), 1);
    assert_eq!(count(&database, "search_index"), 1);
}

#[test]
fn conflicting_id_reuse_is_rejected_and_the_original_is_untouched() {
    let store = Store::new();
    let original = text_capture("original words");
    let first = import_foreground_ingress(&mut store.open(), &original).unwrap();

    let different_text = text_capture("different words");
    let different_timestamp = Capture {
        created_at: "2026-10-08T09:30:09Z".to_string(),
        ..original.clone()
    };
    for conflicting in [different_text, different_timestamp] {
        let error = import_foreground_ingress(&mut store.open(), &conflicting).unwrap_err();
        assert!(matches!(
            error.kind,
            IngressErrorKind::ConflictingReuse { .. }
        ));
        assert_eq!(error.commit_status, CommitStatus::NotCommitted);
    }

    let mut database = store.open();
    let transaction = database.transaction().unwrap();
    assert_eq!(
        get_capture(&transaction, CAPTURE_ID).unwrap(),
        Some(original)
    );
    drop(transaction);
    assert_eq!(count(&database, "items"), 1);
    let retry = import_foreground_ingress(&mut database, &text_capture("original words")).unwrap();
    assert_eq!(retry.item_id, first.item_id);
}

#[test]
fn malformed_records_get_specific_rejections_and_write_nothing() {
    let store = Store::new();
    let base = text_capture("note");
    let cases: Vec<(Capture, IngressRejection)> = vec![
        (
            Capture {
                capture_id: String::new(),
                ..base.clone()
            },
            IngressRejection::MissingField("capture_id"),
        ),
        (
            Capture {
                route_id: String::new(),
                ..base.clone()
            },
            IngressRejection::MissingField("route_id"),
        ),
        (
            Capture {
                text: None,
                audio_reference: None,
                ..base.clone()
            },
            IngressRejection::MissingContent,
        ),
        (
            Capture {
                text: Some(String::new()),
                audio_reference: None,
                ..base.clone()
            },
            IngressRejection::MissingContent,
        ),
        (
            Capture {
                item_scope: "shared".to_string(),
                ..base.clone()
            },
            IngressRejection::UnsupportedScope("shared".to_string()),
        ),
        (
            Capture {
                capture_instant: "yesterday".to_string(),
                ..base.clone()
            },
            IngressRejection::MalformedTimeContext("capture_instant"),
        ),
        (
            Capture {
                created_at: "2026-10-08".to_string(),
                ..base.clone()
            },
            IngressRejection::MalformedTimeContext("created_at"),
        ),
        (
            Capture {
                timezone_id: "Mars/Olympus".to_string(),
                ..base.clone()
            },
            IngressRejection::MalformedTimeContext("timezone_id"),
        ),
        (
            Capture {
                utc_offset_minutes: 24 * 60,
                ..base.clone()
            },
            IngressRejection::MalformedTimeContext("utc_offset_minutes"),
        ),
        (
            Capture {
                audio_reference: Some("../outside/clip.m4a".to_string()),
                ..base.clone()
            },
            IngressRejection::MalformedAudioReference,
        ),
        (
            Capture {
                session_topic: Some(String::new()),
                ..base.clone()
            },
            IngressRejection::MalformedSessionTopic,
        ),
        (
            Capture {
                route_id: "route-missing".to_string(),
                ..base.clone()
            },
            IngressRejection::UnknownRoute("route-missing".to_string()),
        ),
        (
            Capture {
                route_id: "route-work".to_string(),
                ..base.clone()
            },
            IngressRejection::RouteScopeMismatch {
                route_id: "route-work".to_string(),
                route_scope: "work".to_string(),
                item_scope: "personal".to_string(),
            },
        ),
    ];

    for (capture, expected) in cases {
        let mut database = store.open();
        let error = import_foreground_ingress(&mut database, &capture).unwrap_err();
        assert_eq!(validation(&error), &expected);
        assert_eq!(error.commit_status, CommitStatus::NotCommitted);
        assert_nothing_stored(&store.open());
    }
}

#[test]
fn failure_before_commit_rolls_back_everything_and_a_retry_succeeds() {
    for failing_stage in [
        ImportStage::CaptureWritten,
        ImportStage::ItemWritten,
        ImportStage::IndexWritten,
    ] {
        let store = Store::new();
        let capture = text_capture("renew passport");

        let error = import_with_fault_points(&mut store.open(), &capture, &mut |stage| {
            if stage == failing_stage {
                Err(anyhow::anyhow!("injected failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(
            error.commit_status,
            CommitStatus::NotCommitted,
            "{failing_stage:?}"
        );
        assert!(matches!(error.kind, IngressErrorKind::Storage(_)));
        assert_nothing_stored(&store.open());

        let acknowledgment = import_foreground_ingress(&mut store.open(), &capture).unwrap();
        assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);
        let database = store.open();
        assert_eq!(count(&database, "items"), 1);
        assert_eq!(count(&database, "search_index"), 1);
    }
}

#[test]
fn a_presaved_capture_survives_a_failed_import_and_is_imported_on_retry() {
    let store = Store::new();
    let capture = text_capture("sole copy of the source");
    save_capture(&mut store.open(), &capture).unwrap();

    let error = import_with_fault_points(&mut store.open(), &capture, &mut |stage| {
        if stage == ImportStage::IndexWritten {
            Err(anyhow::anyhow!("injected failure"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.commit_status, CommitStatus::NotCommitted);

    let mut database = store.open();
    assert_eq!(count(&database, "captures"), 1);
    assert_eq!(count(&database, "items"), 0);
    let transaction = database.transaction().unwrap();
    assert_eq!(
        get_capture(&transaction, CAPTURE_ID)
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some("sole copy of the source")
    );
    drop(transaction);

    let acknowledgment = import_foreground_ingress(&mut database, &capture).unwrap();
    assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);
    assert_eq!(count(&database, "captures"), 1);
}

#[test]
fn a_presaved_capture_with_unknown_route_stays_and_is_not_acknowledged() {
    let store = Store::new();
    let capture = Capture {
        route_id: "route-missing".to_string(),
        ..text_capture("kept despite rejection")
    };
    save_capture(&mut store.open(), &capture).unwrap();

    let error = import_foreground_ingress(&mut store.open(), &capture).unwrap_err();
    assert!(matches!(
        validation(&error),
        IngressRejection::UnknownRoute(_)
    ));
    let database = store.open();
    assert_eq!(count(&database, "captures"), 1);
    assert_eq!(count(&database, "items"), 0);
}

#[test]
fn a_presaved_capture_with_different_content_conflicts() {
    let store = Store::new();
    save_capture(&mut store.open(), &text_capture("saved first")).unwrap();

    let error =
        import_foreground_ingress(&mut store.open(), &text_capture("arrives second")).unwrap_err();
    assert!(matches!(
        error.kind,
        IngressErrorKind::ConflictingReuse { .. }
    ));
    assert_eq!(count(&store.open(), "items"), 0);
}

#[test]
fn failure_after_commit_reports_committed_and_a_retry_returns_the_same_item() {
    let store = Store::new();
    let capture = text_capture("pick up the parcel");

    let error = import_with_fault_points(&mut store.open(), &capture, &mut |stage| {
        if stage == ImportStage::Committed {
            Err(anyhow::anyhow!("process interrupted after commit"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.commit_status, CommitStatus::Committed);

    let database = store.open();
    assert_eq!(count(&database, "items"), 1);
    assert_eq!(count(&database, "search_index"), 1);
    let committed_item_id: String = database
        .conn()
        .query_row("SELECT item_id FROM items", [], |row| row.get(0))
        .unwrap();
    drop(database);

    let retry = import_foreground_ingress(&mut store.open(), &capture).unwrap();
    assert_eq!(retry.item_id, committed_item_id);
    assert_eq!(retry.disposition, ImportDisposition::AlreadyImported);
    assert_eq!(count(&store.open(), "items"), 1);
}

#[test]
fn a_deleted_item_is_not_recreated_by_redelivery() {
    let store = Store::new();
    let capture = text_capture("to be deleted");
    let acknowledgment = import_foreground_ingress(&mut store.open(), &capture).unwrap();
    store
        .open()
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
            [&acknowledgment.item_id],
        )
        .unwrap();

    let error = import_foreground_ingress(&mut store.open(), &capture).unwrap_err();
    assert!(matches!(
        &error.kind,
        IngressErrorKind::ItemDeleted { item_id } if item_id == &acknowledgment.item_id
    ));
    assert_eq!(count(&store.open(), "items"), 1);
}
