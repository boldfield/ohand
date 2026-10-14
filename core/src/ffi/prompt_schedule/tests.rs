//! Drives the persisted prompt schedule against a migrated store, with a settable clock and a
//! recording stand-in for the OS notification center, directly and through the exports.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, WAIT};
use crate::providers::contracts::ErrorClass;
use crate::reminders::reconcile::{NotificationEffectError, PendingRequest, ScheduleRequest};
use crate::store::schema::SystemClock;
use crate::suggestions::schedule::PromptTime;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    fn new(text: &str) -> TestClock {
        TestClock {
            now: Mutex::new(at(text)),
        }
    }

    fn set(&self, text: &str) {
        *lock(&self.now) = at(text);
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *lock(&self.now)
    }
}

/// The OS notification center: it fires (and forgets) requests on its own, without the app.
#[derive(Default)]
struct Center {
    pending: Vec<(String, DateTime<Utc>)>,
    installs: Vec<String>,
    cancels: Vec<String>,
    fail_list: bool,
}

impl Center {
    fn deliver_through(&mut self, instant: &str) {
        let through = at(instant);
        self.pending.retain(|(_, fire_at)| *fire_at > through);
    }

    fn pending_ids(&self) -> BTreeSet<String> {
        self.pending.iter().map(|(id, _)| id.clone()).collect()
    }

    fn installs_of(&self, id: &str) -> usize {
        self.installs
            .iter()
            .filter(|installed| *installed == id)
            .count()
    }
}

impl NotificationEffects for Center {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.pending
            .retain(|(existing, _)| existing != &request.notification_id);
        self.pending
            .push((request.notification_id.clone(), request.fire_at));
        self.installs.push(request.notification_id.clone());
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.pending
            .retain(|(existing, _)| existing != notification_id);
        self.cancels.push(notification_id.to_string());
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        if self.fail_list {
            return Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "notification_center_failed",
            ));
        }
        Ok(self
            .pending
            .iter()
            .map(|(id, fire_at)| PendingRequest {
                notification_id: id.clone(),
                fire_at: Some(*fire_at),
            })
            .collect())
    }
}

struct Store {
    directory: std::path::PathBuf,
    path: String,
}

impl Store {
    fn fresh(name: &str) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-prompt-schedule-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        Store { directory, path }
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn daily(hour: u8, minute: u8) -> PromptSettings {
    PromptSettings::Daily {
        time: PromptTime::new(hour, minute).unwrap(),
    }
}

fn pass_with(
    database: &mut Database,
    clock: &TestClock,
    center: &mut Center,
    policy: &CapacityPolicy,
    allow: bool,
) -> ReminderPass {
    match reconcile_reminders_and_prompts(database, clock, center, policy, allow) {
        Ok(pass) => pass,
        Err(PassFailure::Reconcile(error)) => panic!("the pass failed: {error:?}"),
        Err(PassFailure::Storage(failure)) => panic!("the pass failed: {failure:?}"),
    }
}

fn pass(
    database: &mut Database,
    clock: &TestClock,
    center: &mut Center,
    allow: bool,
) -> ReminderPass {
    pass_with(database, clock, center, &CapacityPolicy::default(), allow)
}

fn ids(dates: &[&str]) -> BTreeSet<String> {
    dates.iter().map(|date| format!("prompt:{date}")).collect()
}

#[test]
fn a_store_that_was_never_configured_reads_as_off_and_a_pass_touches_no_prompt() {
    let store = Store::fresh("unconfigured");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    center
        .pending
        .push(("prompt:2026-03-10".into(), at("2026-03-10T09:00:00Z")));

    let view = read(&database, &clock).unwrap();
    assert_eq!(
        view,
        ScheduleView {
            enabled: false,
            time: None,
            next: None
        }
    );
    let outcome = pass(&mut database, &clock, &mut center, true);
    assert_eq!(outcome.prompts_installed, 0);
    assert!(
        center.cancels.is_empty(),
        "nothing core never held is cancelled"
    );
    assert!(
        load(&database).unwrap().is_none(),
        "a pass writes no schedule row"
    );
}

#[test]
fn the_schedule_survives_reopening_the_store() {
    let store = Store::fresh("reopen");
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let configured = {
        let database = store.open();
        configure(&database, &clock, daily(9, 30), Tz::UTC).unwrap()
    };
    assert_eq!(
        configured,
        ScheduleView {
            enabled: true,
            time: Some(TimeView {
                hour: 9,
                minute: 30
            }),
            next: Some(NextView {
                local_date: "2026-03-10".parse().unwrap(),
                fire_at: "2026-03-10T09:30:00Z".to_string()
            }),
        }
    );

    let database = store.open();
    assert_eq!(read(&database, &clock).unwrap(), configured);
    let schedule = load(&database).unwrap().expect("the persisted schedule");
    assert_eq!(schedule.settings(), daily(9, 30));
    assert_eq!(schedule.high_water(), Some(at("2026-03-10T08:00:00Z")));
}

#[test]
fn a_prompt_the_os_delivered_while_nothing_was_observing_is_not_reinstalled_after_the_clock_is_set_back(
) {
    let store = Store::fresh("set-back");
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    {
        let mut database = store.open();
        configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();
        let first = pass(&mut database, &clock, &mut center, true);
        assert_eq!(first.prompts_installed, 3);
    }
    assert_eq!(
        center.pending_ids(),
        ids(&["2026-03-10", "2026-03-11", "2026-03-12"])
    );

    // The app is not running while the OS shows the first prompt; the clock is then set back
    // to before the prompt, and the app is opened.
    center.deliver_through("2026-03-10T09:30:00Z");
    clock.set("2026-03-10T07:00:00Z");
    let mut database = store.open();
    refresh(&database, &clock, RefreshTrigger::AppOpen, Tz::UTC).unwrap();
    pass(&mut database, &clock, &mut center, true);

    assert_eq!(
        center.installs_of("prompt:2026-03-10"),
        1,
        "the delivered prompt is not installed again"
    );
    assert_eq!(center.pending_ids(), ids(&["2026-03-11", "2026-03-12"]));
    assert!(load(&database)
        .unwrap()
        .unwrap()
        .is_spent("2026-03-10".parse().unwrap()));
}

#[test]
fn after_a_reopen_and_westward_travel_an_already_held_local_date_gets_no_second_prompt() {
    let store = Store::fresh("westward");
    let clock = TestClock::new("2026-03-09T20:00:00Z");
    let mut center = Center::default();
    {
        let mut database = store.open();
        configure(&database, &clock, daily(9, 0), chrono_tz::Asia::Tokyo).unwrap();
        pass(&mut database, &clock, &mut center, true);
    }
    // 09:00 on 10 March in Tokyo is 00:00 UTC; the prompt fires while the app is closed.
    assert!(center.pending_ids().contains("prompt:2026-03-10"));
    center.deliver_through("2026-03-10T00:30:00Z");

    // The user has flown to Los Angeles, where 10 March 09:00 is still ahead, and opens the app.
    clock.set("2026-03-10T00:30:00Z");
    let mut database = store.open();
    refresh(
        &database,
        &clock,
        RefreshTrigger::AppOpen,
        chrono_tz::America::Los_Angeles,
    )
    .unwrap();
    pass(&mut database, &clock, &mut center, true);

    assert_eq!(center.installs_of("prompt:2026-03-10"), 1);
    assert_eq!(
        center.pending_ids(),
        ids(&["2026-03-11", "2026-03-12", "2026-03-13"])
    );
}

#[test]
fn a_planned_occurrence_that_permission_kept_from_being_attempted_is_not_spent() {
    let store = Store::fresh("permission");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();

    let withheld = pass(&mut database, &clock, &mut center, false);
    assert_eq!(withheld.prompts_withheld, 3);
    assert_eq!(withheld.prompts_installed, 0);
    assert!(center.installs.is_empty());

    clock.set("2026-03-10T09:30:00Z");
    let still_withheld = pass(&mut database, &clock, &mut center, false);
    assert_eq!(still_withheld.prompts_installed, 0);
    let schedule = load(&database).unwrap().unwrap();
    assert!(
        !schedule.is_spent("2026-03-10".parse().unwrap()),
        "nothing was attempted, so the date is not spent"
    );

    configure(&database, &clock, daily(10, 0), Tz::UTC).unwrap();
    let allowed = pass(&mut database, &clock, &mut center, true);
    assert_eq!(allowed.prompts_installed, 3);
    assert!(center
        .pending
        .contains(&("prompt:2026-03-10".to_string(), at("2026-03-10T10:00:00Z"))));
}

#[test]
fn a_planned_occurrence_that_capacity_kept_from_being_attempted_is_not_spent() {
    let store = Store::fresh("capacity");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();

    let no_room = CapacityPolicy {
        max_prompt_requests: 0,
        ..CapacityPolicy::default()
    };
    let deferred = pass_with(&mut database, &clock, &mut center, &no_room, true);
    assert_eq!(deferred.prompts_installed, 0);
    assert_eq!(deferred.capacity.allocation.deferred_prompts.len(), 3);
    assert!(center.installs.is_empty());

    clock.set("2026-03-10T09:30:00Z");
    pass_with(&mut database, &clock, &mut center, &no_room, true);
    assert!(!load(&database)
        .unwrap()
        .unwrap()
        .is_spent("2026-03-10".parse().unwrap()));

    configure(&database, &clock, daily(10, 0), Tz::UTC).unwrap();
    let allowed = pass(&mut database, &clock, &mut center, true);
    assert_eq!(allowed.prompts_installed, 3);
    assert!(center
        .pending
        .contains(&("prompt:2026-03-10".to_string(), at("2026-03-10T10:00:00Z"))));
}

#[test]
fn a_dst_gap_fires_at_the_first_valid_minute_and_a_fold_at_its_first_occurrence() {
    let store = Store::fresh("dst");
    let database = store.open();

    let before_gap = TestClock::new("2026-03-07T12:00:00Z");
    let gap = configure(
        &database,
        &before_gap,
        daily(2, 30),
        chrono_tz::America::New_York,
    )
    .unwrap();
    let next = gap.next.expect("a planned occurrence");
    assert_eq!(next.local_date.to_string(), "2026-03-08");
    assert_eq!(
        next.fire_at, "2026-03-08T07:00:00Z",
        "03:00 EDT, the first valid minute"
    );

    configure(&database, &before_gap, PromptSettings::Off, Tz::UTC).unwrap();
    let before_fold = TestClock::new("2026-10-31T12:00:00Z");
    let fold = configure(
        &database,
        &before_fold,
        daily(1, 30),
        chrono_tz::America::New_York,
    )
    .unwrap();
    let next = fold.next.expect("a planned occurrence");
    assert_eq!(next.local_date.to_string(), "2026-11-01");
    assert_eq!(
        next.fire_at, "2026-11-01T05:30:00Z",
        "the first 01:30, in EDT"
    );
}

#[test]
fn repeated_refreshes_and_passes_do_not_duplicate_an_install() {
    let store = Store::fresh("repeat");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();

    for trigger in [
        RefreshTrigger::AppOpen,
        RefreshTrigger::Reentry,
        RefreshTrigger::ClockOrZoneChange,
        RefreshTrigger::AppOpen,
    ] {
        refresh(&database, &clock, trigger, Tz::UTC).unwrap();
        pass(&mut database, &clock, &mut center, true);
        pass(&mut database, &clock, &mut center, true);
    }

    assert_eq!(center.installs.len(), 3, "each prompt is installed once");
    assert_eq!(
        center.pending_ids(),
        ids(&["2026-03-10", "2026-03-11", "2026-03-12"])
    );
}

#[test]
fn turning_prompts_off_or_withholding_them_cancels_the_pending_ones_without_spending_them() {
    let store = Store::fresh("withhold");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();
    pass(&mut database, &clock, &mut center, true);
    assert_eq!(center.pending.len(), 3);

    let withheld = pass(&mut database, &clock, &mut center, false);
    assert_eq!(withheld.prompts_withheld, 3);
    assert!(
        center.pending.is_empty(),
        "no prompt occupies a slot without permission"
    );

    let restored = pass(&mut database, &clock, &mut center, true);
    assert_eq!(
        restored.prompts_installed, 3,
        "withholding released the dates"
    );

    configure(&database, &clock, PromptSettings::Off, Tz::UTC).unwrap();
    pass(&mut database, &clock, &mut center, true);
    assert!(
        center.pending.is_empty(),
        "turning prompts off cancels them"
    );
    assert_eq!(
        read(&database, &clock).unwrap(),
        ScheduleView {
            enabled: false,
            time: None,
            next: None
        }
    );
}

#[test]
fn a_failed_listing_still_persists_the_clock_reading() {
    let store = Store::fresh("failed-listing");
    let mut database = store.open();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    let mut center = Center::default();
    configure(&database, &clock, daily(9, 0), Tz::UTC).unwrap();
    pass(&mut database, &clock, &mut center, true);

    clock.set("2026-03-10T09:30:00Z");
    center.fail_list = true;
    let failed = reconcile_reminders_and_prompts(
        &mut database,
        &clock,
        &mut center,
        &CapacityPolicy::default(),
        true,
    );
    assert!(matches!(failed, Err(PassFailure::Reconcile(_))));
    let schedule = load(&database).unwrap().unwrap();
    assert_eq!(schedule.high_water(), Some(at("2026-03-10T09:30:00Z")));
    assert!(
        schedule.is_spent("2026-03-10".parse().unwrap()),
        "the instant of the held prompt was lived through"
    );
}

#[test]
fn a_corrupt_stored_schedule_is_a_storage_failure_not_an_empty_schedule() {
    let store = Store::fresh("corrupt");
    let database = store.open();
    database
        .conn()
        .execute(
            "INSERT INTO prompt_schedule (singleton, schedule, updated_at)
             VALUES (1, 'not json', '2026-03-10T08:00:00Z')",
            [],
        )
        .unwrap();
    let clock = TestClock::new("2026-03-10T08:00:00Z");
    assert!(read(&database, &clock).is_err());
    assert!(configure(&database, &clock, daily(9, 0), Tz::UTC).is_err());
}

fn export_call(
    handle: OhandCoreHandle,
    operation_id: u64,
    which: &str,
    request: &Value,
) -> crate::ffi::core_handle::tests::Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        match which {
            "configure" => ohand_core_start_configure_prompt_schedule(
                handle,
                operation_id,
                bytes.as_ptr(),
                bytes.len(),
            ),
            "refresh" => ohand_core_start_refresh_prompt_schedule(
                handle,
                operation_id,
                bytes.as_ptr(),
                bytes.len(),
            ),
            other => panic!("unknown export {other}"),
        }
    })
}

#[test]
fn the_exports_configure_refresh_and_read_the_schedule_across_a_reopen() {
    let _guard = serial();
    let store = Store::fresh("exports");
    let open = || {
        let mut handle = 0;
        let outcome =
            consume(unsafe { ohand_core_open(store.path.as_ptr(), store.path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        handle
    };
    let handle = open();
    let (recorder, events) = recorder();
    register(handle, &recorder);

    let configured = export_call(
        handle,
        1,
        "configure",
        &json!({"time_zone": "UTC", "settings": {"mode": "daily", "time": {"hour": 9, "minute": 15}}}),
    );
    assert_eq!(configured.status, OHAND_CORE_STATUS_OK);
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.status, OHAND_CORE_STATUS_OK);
    assert_ne!(event.thread, std::thread::current().id());
    let body: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(body["operation_id"], 1);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["time"], json!({"hour": 9, "minute": 15}));
    assert!(body["next"]["fire_at"]
        .as_str()
        .unwrap()
        .ends_with("T09:15:00Z"));
    assert!(body["next"]["local_date"].as_str().is_some());

    let refreshed = export_call(
        handle,
        2,
        "refresh",
        &json!({"time_zone": "Europe/Paris", "trigger": "foreground"}),
    );
    assert_eq!(refreshed.status, OHAND_CORE_STATUS_OK);
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.status, OHAND_CORE_STATUS_OK);
    let refreshed: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(refreshed["enabled"], true);
    assert_eq!(refreshed["time"], body["time"]);

    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    drop(recorder);

    let handle = open();
    let (recorder, events) = crate::ffi::core_handle::tests::recorder();
    register(handle, &recorder);
    assert_eq!(
        consume(ohand_core_start_read_prompt_schedule(handle, 3)).status,
        OHAND_CORE_STATUS_OK
    );
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    let read_back: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(read_back["operation_id"], 3);
    assert_eq!(read_back["enabled"], true);
    assert_eq!(read_back["time"], json!({"hour": 9, "minute": 15}));
    assert_eq!(read_back["next"], body["next"]);

    let off = export_call(
        handle,
        4,
        "configure",
        &json!({"time_zone": "UTC", "settings": {"mode": "off"}}),
    );
    assert_eq!(off.status, OHAND_CORE_STATUS_OK);
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    let off: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(
        off,
        json!({"operation_id": 4, "enabled": false, "time": null, "next": null})
    );
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    drop(recorder);
}

#[test]
fn bad_requests_are_refused_with_normalized_failures_and_change_nothing() {
    let _guard = serial();
    let store = Store::fresh("invalid");
    let mut handle = 0;
    let outcome =
        consume(unsafe { ohand_core_open(store.path.as_ptr(), store.path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);

    let daily_nine = json!({"mode": "daily", "time": {"hour": 9, "minute": 0}});
    let invalid = [
        (
            "configure",
            json!({"time_zone": "Not/AZone", "settings": daily_nine}),
        ),
        (
            "configure",
            json!({"time_zone": "", "settings": daily_nine}),
        ),
        (
            "configure",
            json!({"time_zone": "UTC", "settings": {"mode": "daily", "time": {"hour": 24, "minute": 0}}}),
        ),
        (
            "configure",
            json!({"time_zone": "UTC", "settings": {"mode": "daily", "time": {"hour": 9, "minute": 60}}}),
        ),
        (
            "configure",
            json!({"time_zone": "UTC", "settings": {"mode": "weekly"}}),
        ),
        (
            "configure",
            json!({"time_zone": "UTC", "settings": daily_nine, "extra": 1}),
        ),
        ("configure", json!({"settings": daily_nine})),
        (
            "refresh",
            json!({"time_zone": "UTC", "trigger": "settings_change"}),
        ),
        (
            "refresh",
            json!({"time_zone": "Nowhere", "trigger": "launch"}),
        ),
        ("refresh", json!({"trigger": "launch"})),
    ];
    for (which, request) in &invalid {
        let outcome = export_call(handle, 9, which, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }

    let empty =
        consume(unsafe { ohand_core_start_configure_prompt_schedule(handle, 9, b"".as_ptr(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe {
        ohand_core_start_refresh_prompt_schedule(handle, 9, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let oversized = vec![b' '; OHAND_CORE_MAX_PROMPT_SCHEDULE_REQUEST_BYTES + 1];
    let large = consume(unsafe {
        ohand_core_start_configure_prompt_schedule(handle, 9, oversized.as_ptr(), oversized.len())
    });
    assert_eq!(large.code(), "request_too_large");
    let unknown = consume(unsafe {
        ohand_core_start_refresh_prompt_schedule(u64::MAX, 9, b"{}".as_ptr(), 2)
    });
    assert_eq!(unknown.code(), "invalid_handle");
    let unknown_read = consume(ohand_core_start_read_prompt_schedule(u64::MAX, 9));
    assert_eq!(unknown_read.code(), "invalid_handle");

    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let database = store.open();
    assert!(
        load(&database).unwrap().is_none(),
        "no refused request wrote a schedule"
    );
}
