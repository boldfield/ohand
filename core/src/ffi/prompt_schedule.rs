//! The optional daily prompt schedule over the core handle (S05a).
//!
//! The core is the only holder of S02's [`PromptSchedule`]: it lives in the single-row
//! `prompt_schedule` table, ledger included, and survives relaunches. Native code supplies only
//! the IANA time zone, the notification permission and the triggers; every operation reads the
//! core clock itself and persists the schedule before its outcome event is delivered.
//!
//! * `ohand_core_start_configure_prompt_schedule` applies the user's choice. The request is
//!   `{"time_zone","settings"}` where `settings` is `{"mode":"off"}` or
//!   `{"mode":"daily","time":{"hour","minute"}}` (local wall-clock time, hour 0-23, minute 0-59).
//! * `ohand_core_start_refresh_prompt_schedule` looks at the schedule again. The request is
//!   `{"time_zone","trigger"}` where `trigger` is `launch` (the only one that refills the
//!   horizon), `foreground`, or `time_or_zone_change` (these two only drop occurrences). A change
//!   of the setting is `configure`, not a refresh.
//! * `ohand_core_start_read_prompt_schedule` reads the current settings and next planned
//!   occurrence without changing anything.
//!
//! The outcome event of each is `{"operation_id","enabled","time":{"hour","minute"}|null,
//! "next":{"local_date","fire_at"}|null}`. `next` is the soonest planned occurrence still ahead of
//! the core clock on an unspent date. There is no content: prompts are generic.
//!
//! None of these installs anything. Installs happen inside the reminder pass
//! (`ohand_core_start_reconcile_reminders`), which takes its prompt demands only from this
//! persisted schedule; see [`reconcile_reminders_and_prompts`].

use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::reminders::capacity::{reconcile_with_capacity, CapacityPolicy, CapacityReport};
use crate::reminders::reconcile::{NotificationEffects, ReconcileError};
use crate::store::schema::{Clock, Database};
use crate::suggestions::schedule::{
    cancel_prompts, prompt_local_date, reconcile_prompts, PromptSchedule, PromptSettings,
    RefreshTrigger,
};
use anyhow::Context;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Longest request accepted, in bytes.
pub const OHAND_CORE_MAX_PROMPT_SCHEDULE_REQUEST_BYTES: usize = 4_096;
/// Longest time zone identifier accepted, in bytes.
const MAX_TIME_ZONE_BYTES: usize = 64;

fn storage_failure(error: impl Into<anyhow::Error>) -> AbiFailure {
    AbiFailure::from_store(&error.into(), AbiFailure::STORAGE_ERROR)
}

/// The persisted schedule, or `None` when prompting has never been configured.
pub(super) fn load(database: &Database) -> Result<Option<PromptSchedule>, AbiFailure> {
    let stored: Option<String> = database
        .conn()
        .query_row(
            "SELECT schedule FROM prompt_schedule WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("reading the prompt schedule")
        .map_err(storage_failure)?;
    stored
        .map(|text| {
            serde_json::from_str(&text)
                .context("decoding the prompt schedule")
                .map_err(storage_failure)
        })
        .transpose()
}

pub(super) fn save(
    database: &Database,
    schedule: &PromptSchedule,
    now: DateTime<Utc>,
) -> Result<(), AbiFailure> {
    let text = serde_json::to_string(schedule).map_err(|_| AbiFailure::INTERNAL)?;
    database
        .conn()
        .execute(
            "INSERT INTO prompt_schedule (singleton, schedule, updated_at) VALUES (1, ?1, ?2)
             ON CONFLICT(singleton) DO UPDATE SET schedule = ?1, updated_at = ?2",
            rusqlite::params![text, now.to_rfc3339_opts(SecondsFormat::Secs, true)],
        )
        .context("saving the prompt schedule")
        .map_err(storage_failure)?;
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct TimeView {
    hour: u8,
    minute: u8,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct NextView {
    local_date: NaiveDate,
    fire_at: String,
}

/// What a caller may see of the schedule: whether it is on, the chosen time and the next planned
/// occurrence. No content.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct ScheduleView {
    enabled: bool,
    time: Option<TimeView>,
    next: Option<NextView>,
}

fn view_of(schedule: &PromptSchedule, now: DateTime<Utc>) -> ScheduleView {
    let time = match schedule.settings() {
        PromptSettings::Off => None,
        PromptSettings::Daily { time } => Some(TimeView {
            hour: time.hour(),
            minute: time.minute(),
        }),
    };
    let next = schedule.demands(now).into_iter().next().and_then(|demand| {
        Some(NextView {
            local_date: prompt_local_date(&demand.notification_id)?,
            fire_at: demand.fire_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        })
    });
    ScheduleView {
        enabled: schedule.is_enabled(),
        time,
        next,
    }
}

/// Applies `settings` with the core clock and the caller's zone, and persists the schedule.
pub(super) fn configure(
    database: &Database,
    clock: &dyn Clock,
    settings: PromptSettings,
    zone: Tz,
) -> Result<ScheduleView, AbiFailure> {
    let now = clock.now();
    let mut schedule = load(database)?.unwrap_or_default();
    schedule.configure(settings, now, zone);
    save(database, &schedule, now)?;
    Ok(view_of(&schedule, now))
}

/// Looks at the schedule again for `trigger` with the core clock and the caller's zone, and
/// persists it. A schedule that was never configured stays unconfigured but records the clock
/// reading, like every other entry point that takes `now`.
pub(super) fn refresh(
    database: &Database,
    clock: &dyn Clock,
    trigger: RefreshTrigger,
    zone: Tz,
) -> Result<ScheduleView, AbiFailure> {
    let now = clock.now();
    let mut schedule = load(database)?.unwrap_or_default();
    schedule.refresh(trigger, now, zone);
    save(database, &schedule, now)?;
    Ok(view_of(&schedule, now))
}

pub(super) fn read(database: &Database, clock: &dyn Clock) -> Result<ScheduleView, AbiFailure> {
    let schedule = load(database)?.unwrap_or_default();
    Ok(view_of(&schedule, clock.now()))
}

/// Why a reminder pass failed: the pass itself, or reading or writing the persisted schedule.
pub(super) enum PassFailure {
    Reconcile(ReconcileError),
    Storage(AbiFailure),
}

/// What a pass did beyond the reminders' own report.
pub(super) struct ReminderPass {
    pub capacity: CapacityReport,
    pub prompts_installed: usize,
    /// Prompt installs and cancels that failed, on top of the capacity report's own failures.
    pub prompt_failures: usize,
    /// Planned occurrences the pass did not offer because the permission withheld them.
    pub prompts_withheld: usize,
}

/// One reminder pass whose optional prompts come only from the persisted schedule.
///
/// With `allow_optional_prompts` and an enabled schedule, [`reconcile_prompts`] runs the capacity
/// pass (reminders first, prompts only in the slots they leave) and installs the granted prompts.
/// In every other case a schedule that has ever been configured has its pending prompts cancelled
/// first, and they are released rather than spent, so they never take a slot from a reminder and
/// come back when the permission or the setting allows; the reminders then run alone. A store
/// that never configured prompting is not read beyond the empty row and no prompt is cancelled.
///
/// The schedule is persisted before the pass returns, whether the pass succeeded or failed: its
/// ledger holds the evidence of what the OS may have been asked to do.
pub(super) fn reconcile_reminders_and_prompts(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    policy: &CapacityPolicy,
    allow_optional_prompts: bool,
) -> Result<ReminderPass, PassFailure> {
    let pass_clock = PassClock { now: clock.now() };
    let Some(mut schedule) = load(database).map_err(PassFailure::Storage)? else {
        let capacity = reconcile_with_capacity(database, &pass_clock, effects, policy, &[])
            .map_err(PassFailure::Reconcile)?;
        return Ok(ReminderPass {
            capacity,
            prompts_installed: 0,
            prompt_failures: 0,
            prompts_withheld: 0,
        });
    };
    let before = schedule.clone();
    let offered = schedule.demands(pass_clock.now).len();
    let outcome = run_pass(
        database,
        &pass_clock,
        effects,
        policy,
        &mut schedule,
        allow_optional_prompts,
    );
    if schedule != before {
        save(database, &schedule, pass_clock.now).map_err(PassFailure::Storage)?;
    }
    let mut pass = outcome.map_err(PassFailure::Reconcile)?;
    if schedule.is_enabled() && !allow_optional_prompts {
        pass.prompts_withheld = offered;
    }
    Ok(pass)
}

fn run_pass(
    database: &mut Database,
    pass_clock: &PassClock,
    effects: &mut dyn NotificationEffects,
    policy: &CapacityPolicy,
    schedule: &mut PromptSchedule,
    allow_optional_prompts: bool,
) -> Result<ReminderPass, ReconcileError> {
    if allow_optional_prompts && schedule.is_enabled() {
        let mut report = reconcile_prompts(database, pass_clock, effects, schedule, policy)?;
        if let Some(capacity) = report.capacity.take() {
            return Ok(ReminderPass {
                // The capacity report already counts its own cancel failures.
                prompt_failures: report
                    .failures
                    .len()
                    .saturating_sub(capacity.prompt_cancel_failures.len()),
                prompts_installed: report.installed.len(),
                prompts_withheld: 0,
                capacity,
            });
        }
    }
    let cancelled = cancel_prompts(pass_clock, effects, schedule)?;
    let capacity = reconcile_with_capacity(database, pass_clock, effects, policy, &[])?;
    Ok(ReminderPass {
        capacity,
        prompts_installed: 0,
        prompt_failures: cancelled.failures.len(),
        prompts_withheld: 0,
    })
}

/// One instant for the whole pass: the schedule's ledger and the capacity pass agree on what is
/// past.
struct PassClock {
    now: DateTime<Utc>,
}

impl Clock for PassClock {
    fn now(&self) -> DateTime<Utc> {
        self.now
    }
}

fn parse_zone(time_zone: &str) -> Result<Tz, AbiFailure> {
    if time_zone.is_empty() || time_zone.len() > MAX_TIME_ZONE_BYTES {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    time_zone
        .parse::<Tz>()
        .map_err(|_| AbiFailure::INVALID_REQUEST)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureRequest {
    time_zone: String,
    settings: PromptSettings,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TriggerName {
    Launch,
    Foreground,
    TimeOrZoneChange,
}

impl From<TriggerName> for RefreshTrigger {
    fn from(name: TriggerName) -> RefreshTrigger {
        match name {
            TriggerName::Launch => RefreshTrigger::AppOpen,
            TriggerName::Foreground => RefreshTrigger::Reentry,
            TriggerName::TimeOrZoneChange => RefreshTrigger::ClockOrZoneChange,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshRequest {
    time_zone: String,
    trigger: TriggerName,
}

#[derive(Serialize)]
struct ScheduleOutcome {
    operation_id: u64,
    #[serde(flatten)]
    view: ScheduleView,
}

fn outcome_bytes(operation_id: u64, view: ScheduleView) -> Result<Vec<u8>, AbiFailure> {
    serde_json::to_vec(&ScheduleOutcome { operation_id, view }).map_err(|_| AbiFailure::INTERNAL)
}

fn parse_request<T: for<'de> Deserialize<'de>>(
    request: *const u8,
    request_len: usize,
) -> Result<T, AbiFailure> {
    if request_len > OHAND_CORE_MAX_PROMPT_SCHEDULE_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if request_len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if request.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    // SAFETY: the caller promised `request_len` readable bytes at `request`.
    let bytes = unsafe { std::slice::from_raw_parts(request, request_len) };
    serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)
}

/// Queues the user's choice described by the `request_len` JSON bytes at `request` (see the
/// module documentation). The outcome event for `operation_id` is the schedule as persisted, or
/// a normalized failure (`invalid_request` for an unknown zone, an out-of-range time or an
/// unknown field).
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_configure_prompt_schedule(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let parsed: ConfigureRequest = parse_request(request, request_len)?;
        let zone = parse_zone(&parsed.time_zone)?;
        let settings = parsed.settings;
        let job: JobFn = Box::new(move |database: &Mutex<Database>| {
            let database = lock(database);
            let view = configure(
                &database,
                &crate::store::schema::SystemClock,
                settings,
                zone,
            )?;
            outcome_bytes(operation_id, view)
        });
        core.submit(operation_id, job)
    })
}

/// Queues a look at the schedule for the trigger described by the `request_len` JSON bytes at
/// `request` (see the module documentation). The outcome event for `operation_id` is the
/// schedule as persisted, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_refresh_prompt_schedule(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let parsed: RefreshRequest = parse_request(request, request_len)?;
        let zone = parse_zone(&parsed.time_zone)?;
        let trigger = RefreshTrigger::from(parsed.trigger);
        let job: JobFn = Box::new(move |database: &Mutex<Database>| {
            let database = lock(database);
            let view = refresh(&database, &crate::store::schema::SystemClock, trigger, zone)?;
            outcome_bytes(operation_id, view)
        });
        core.submit(operation_id, job)
    })
}

/// Queues a read of the schedule. The outcome event for `operation_id` is the current settings
/// and next planned occurrence, or a normalized failure. Reading changes nothing.
#[no_mangle]
pub extern "C" fn ohand_core_start_read_prompt_schedule(
    handle: OhandCoreHandle,
    operation_id: u64,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let job: JobFn = Box::new(move |database: &Mutex<Database>| {
            let database = lock(database);
            let view = read(&database, &crate::store::schema::SystemClock)?;
            outcome_bytes(operation_id, view)
        });
        core.submit(operation_id, job)
    })
}

#[cfg(test)]
mod tests;
