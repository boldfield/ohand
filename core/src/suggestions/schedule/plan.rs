use std::collections::BTreeSet;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::reminders::capacity::{
    CapacityReport, PromptDemand, MAX_PROMPT_REQUESTS, PROMPT_HORIZON,
};

/// The most daily prompts planned at once: the next three occurrences of the chosen time. After
/// that many days without opening the app the OS holds no prompt at all, so there is no backlog.
pub const PROMPT_HORIZON_DAYS: usize = 3;

/// Every prompt identifier starts with this prefix and has no `#`, so it can never equal a
/// reminder's `reminder_id#generation` identifier.
pub const PROMPT_ID_PREFIX: &str = "prompt:";

const _: () = assert!(PROMPT_HORIZON_DAYS <= MAX_PROMPT_REQUESTS);
const _: () = assert!(
    Duration::days(PROMPT_HORIZON_DAYS as i64 + 1).num_seconds() <= PROMPT_HORIZON.num_seconds()
);

/// Longest DST gap searched for the first valid local minute after a skipped time.
const MAX_GAP_MINUTES: i64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PromptScheduleError {
    #[error("prompt time must be a valid hour (0-23) and minute (0-59)")]
    InvalidTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RawPromptTime {
    hour: u8,
    minute: u8,
}

/// A chosen local wall-clock time of day, to the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPromptTime", into = "RawPromptTime")]
pub struct PromptTime {
    hour: u8,
    minute: u8,
}

impl PromptTime {
    pub fn new(hour: u8, minute: u8) -> Result<PromptTime, PromptScheduleError> {
        if hour > 23 || minute > 59 {
            return Err(PromptScheduleError::InvalidTime);
        }
        Ok(PromptTime { hour, minute })
    }

    pub fn hour(&self) -> u8 {
        self.hour
    }

    pub fn minute(&self) -> u8 {
        self.minute
    }
}

impl TryFrom<RawPromptTime> for PromptTime {
    type Error = PromptScheduleError;

    fn try_from(raw: RawPromptTime) -> Result<PromptTime, PromptScheduleError> {
        PromptTime::new(raw.hour, raw.minute)
    }
}

impl From<PromptTime> for RawPromptTime {
    fn from(time: PromptTime) -> RawPromptTime {
        RawPromptTime {
            hour: time.hour,
            minute: time.minute,
        }
    }
}

/// Whether optional daily prompting is on. Off until a time is chosen: `Daily` cannot be built
/// without a [`PromptTime`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PromptSettings {
    #[default]
    Off,
    Daily {
        time: PromptTime,
    },
}

/// What causes the schedule to be looked at again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// The app was opened (cold launch). The only trigger that refills the horizon.
    AppOpen,
    /// The app returned to the foreground.
    Reentry,
    /// The system clock or time zone changed.
    ClockOrZoneChange,
}

/// One planned local-date occurrence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedPrompt {
    pub notification_id: String,
    pub local_date: NaiveDate,
    pub fire_at: DateTime<Utc>,
    /// The OS was last seen holding this request at `fire_at`, and no capacity pass has since
    /// reported cancelling it. If it is later absent while `fire_at` still looks ahead of the
    /// clock and a slot is free for it, it fired and the clock moved back.
    #[serde(default)]
    pub installed: bool,
}

/// The bounded optional daily prompt schedule: the user's setting and the occurrences planned
/// from it. A plain value. The caller keeps it with the app's other configuration (it
/// serializes to JSON) and hands it to `reconcile_prompts` to bring the OS in line.
///
/// Rules:
/// - Off by default; a daily prompt exists only after a local time has been chosen.
/// - At most [`PROMPT_HORIZON_DAYS`] occurrences are planned, always in the future. The
///   horizon is refilled only by [`RefreshTrigger::AppOpen`] or by the user changing the
///   setting, which happens inside the open app. Re-entry and clock or zone changes only drop
///   occurrences that have passed; they never add one.
/// - A passed occurrence is never replayed or made up, so nonuse leaves nothing queued and the
///   next open starts from the next future occurrence.
/// - Passing is remembered: `last_elapsed` is the latest planned instant seen to have passed,
///   either by the clock or because an installed request vanished from the OS before its
///   instant (it fired and the clock was moved back). No occurrence at or before it is planned
///   again, so rolling the clock back cannot replay a prompt that already fired. It is bounded
///   by the horizon, never by the clock reading itself.
/// - Absence from the OS is taken for delivery only when core did not cause it. A prompt that
///   core's capacity rule cancelled or holds back for explicit reminders is never fenced: the
///   reconcile pass checks its own allocation, and a capacity pass run elsewhere reports its
///   cancellations through [`PromptSchedule::record_capacity_pass`]. Only the leading run of
///   vanished occurrences counts, so the fence never passes a prompt the OS still holds. A
///   cancellation core cannot see (a permission toggle, an OS purge) can thus cost at most the
///   planned horizon of prompts, which is acceptable for an optional feature; it never cancels a
///   pending one.
/// - The identifier of an occurrence is `prompt:<local date>`: stable, so a re-plan replaces
///   rather than duplicates, and it carries no item content.
/// - Local-time semantics: a skipped local time (DST gap) fires at the first valid minute after
///   the gap; a repeated local time (DST fold) fires at its first occurrence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSchedule {
    settings: PromptSettings,
    planned: Vec<PlannedPrompt>,
    #[serde(default)]
    last_elapsed: Option<DateTime<Utc>>,
}

impl PromptSchedule {
    pub fn settings(&self) -> PromptSettings {
        self.settings
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.settings, PromptSettings::Daily { .. })
    }

    pub fn planned(&self) -> &[PlannedPrompt] {
        &self.planned
    }

    /// Apply the user's choice. Off clears every planned prompt; a time rebuilds the horizon
    /// from `now`, so an earlier time today that has already passed is not replayed.
    pub fn configure(&mut self, settings: PromptSettings, now: DateTime<Utc>, zone: Tz) {
        self.retire_elapsed(now);
        self.settings = settings;
        self.rebuild(now, zone);
    }

    pub fn refresh(&mut self, trigger: RefreshTrigger, now: DateTime<Utc>, zone: Tz) {
        if !self.is_enabled() {
            self.planned.clear();
            return;
        }
        self.retire_elapsed(now);
        if trigger == RefreshTrigger::AppOpen {
            self.rebuild(now, zone);
        }
    }

    /// Replace the plan with a fresh fill. An occurrence that was already planned at the same
    /// instant keeps its `installed` mark, so a refill that is the first observation after a
    /// clock rollback can still tell that the OS fired it.
    fn rebuild(&mut self, now: DateTime<Utc>, zone: Tz) {
        let mut planned = fill(self.settings, now, zone, self.last_elapsed);
        for prompt in &mut planned {
            prompt.installed = self.planned.iter().any(|previous| {
                previous.installed
                    && previous.notification_id == prompt.notification_id
                    && previous.fire_at == prompt.fire_at
            });
        }
        self.planned = planned;
    }

    /// Drop occurrences whose instant has passed by the clock and remember the latest.
    pub(super) fn retire_elapsed(&mut self, now: DateTime<Utc>) {
        self.retire_where(|prompt| prompt.fire_at <= now);
    }

    /// Retire occurrences the clock says have passed, and the leading run of installed ones that
    /// the OS no longer holds although this pass's capacity allocation grants them a slot: those
    /// fired before the clock was moved back. Absence alone is not delivery. A prompt the
    /// capacity rule holds back (because explicit reminders need its slot) is absent because
    /// core cancelled it, so it is kept and reinstalled once a slot frees. The run also stops at
    /// the first occurrence still pending (or never installed), so the refill floor never passes
    /// a prompt that is still valid. Called by the reconcile pass with the identifiers the OS
    /// currently holds and the prompts the capacity pass granted. Returns whether an occurrence
    /// the clock has not reached was retired.
    pub(super) fn retire_fired(
        &mut self,
        now: DateTime<Utc>,
        pending: &BTreeSet<&str>,
        granted: &BTreeSet<&str>,
    ) -> bool {
        self.planned.sort_by_key(|prompt| prompt.fire_at);
        let mut run_open = true;
        let mut retired_ahead_of_clock = false;
        self.retire_where(|prompt| {
            let vanished = prompt.installed
                && !pending.contains(prompt.notification_id.as_str())
                && granted.contains(prompt.notification_id.as_str());
            run_open = run_open && (prompt.fire_at <= now || vanished);
            retired_ahead_of_clock |= run_open && prompt.fire_at > now;
            run_open
        });
        retired_ahead_of_clock
    }

    fn retire_where(&mut self, mut has_fired: impl FnMut(&PlannedPrompt) -> bool) {
        let mut latest = self.last_elapsed;
        self.planned.retain(|prompt| {
            if !has_fired(prompt) {
                return true;
            }
            latest = latest.max(Some(prompt.fire_at));
            false
        });
        self.last_elapsed = latest;
    }

    /// Record which planned prompts the OS holds after a reconcile pass.
    pub(super) fn record_installed(&mut self, held: &BTreeSet<&str>) {
        for prompt in &mut self.planned {
            prompt.installed = held.contains(prompt.notification_id.as_str());
        }
    }

    /// Record a capacity pass that ran outside [`reconcile_prompts`](super::reconcile_prompts)
    /// with this schedule's [`demands`](Self::demands), such as the reminder side making room
    /// for a new reminder. Prompts it cancelled or held back are no longer marked installed, so
    /// their absence from the OS is never taken for a delivery, whatever capacity is free when
    /// the prompts are next reconciled. Every caller that offers these demands to a capacity
    /// pass must hand its report back here.
    pub fn record_capacity_pass(&mut self, report: &CapacityReport) {
        let displaced: BTreeSet<&str> = report
            .prompts_cancelled
            .iter()
            .chain(
                report
                    .allocation
                    .deferred_prompts
                    .iter()
                    .map(|deferred| &deferred.notification_id),
            )
            .map(String::as_str)
            .collect();
        for prompt in &mut self.planned {
            if displaced.contains(prompt.notification_id.as_str()) {
                prompt.installed = false;
            }
        }
    }

    /// The prompt candidates to offer the capacity pass: none unless enabled, only future
    /// occurrences, soonest first and never more than the horizon.
    pub fn demands(&self, now: DateTime<Utc>) -> Vec<PromptDemand> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let mut upcoming: Vec<&PlannedPrompt> = self
            .planned
            .iter()
            .filter(|prompt| {
                prompt.fire_at > now && prompt.notification_id.starts_with(PROMPT_ID_PREFIX)
            })
            .collect();
        upcoming.sort_by(|left, right| {
            (left.fire_at, &left.notification_id).cmp(&(right.fire_at, &right.notification_id))
        });
        upcoming.dedup_by(|later, earlier| later.notification_id == earlier.notification_id);
        upcoming
            .into_iter()
            .take(PROMPT_HORIZON_DAYS)
            .map(|prompt| PromptDemand {
                notification_id: prompt.notification_id.clone(),
                fire_at: prompt.fire_at,
            })
            .collect()
    }
}

/// The identifier of the occurrence on `local_date`.
pub fn prompt_notification_id(local_date: NaiveDate) -> String {
    format!("{PROMPT_ID_PREFIX}{local_date}")
}

/// Whether `notification_id` names a daily prompt rather than a reminder or anything else.
pub fn is_prompt_notification_id(notification_id: &str) -> bool {
    notification_id.starts_with(PROMPT_ID_PREFIX) && !notification_id.contains('#')
}

fn fill(
    settings: PromptSettings,
    now: DateTime<Utc>,
    zone: Tz,
    last_elapsed: Option<DateTime<Utc>>,
) -> Vec<PlannedPrompt> {
    let PromptSettings::Daily { time } = settings else {
        return Vec::new();
    };
    let mut planned = Vec::with_capacity(PROMPT_HORIZON_DAYS);
    let floor = last_elapsed.map_or(now, |elapsed| elapsed.max(now));
    let mut local_date = floor.with_timezone(&zone).date_naive();
    // Today's occurrence may already be past, and a gap can push one past `now` again, so scan
    // a couple of extra dates rather than assuming the first `PROMPT_HORIZON_DAYS` qualify.
    for _ in 0..PROMPT_HORIZON_DAYS + 2 {
        if planned.len() == PROMPT_HORIZON_DAYS {
            break;
        }
        if let Some(fire_at) = local_fire_instant(local_date, time, zone) {
            if fire_at > floor {
                planned.push(PlannedPrompt {
                    notification_id: prompt_notification_id(local_date),
                    local_date,
                    fire_at,
                    installed: false,
                });
            }
        }
        local_date = local_date.succ_opt().unwrap_or(local_date);
    }
    planned
}

/// The UTC instant of `time` on `local_date` in `zone`, resolving DST gaps forward and folds
/// to the first occurrence.
pub fn local_fire_instant(
    local_date: NaiveDate,
    time: PromptTime,
    zone: Tz,
) -> Option<DateTime<Utc>> {
    let wall = local_date.and_time(NaiveTime::from_hms_opt(
        u32::from(time.hour),
        u32::from(time.minute),
        0,
    )?);
    (0..=MAX_GAP_MINUTES).find_map(|shift| {
        match zone.from_local_datetime(&(wall + Duration::minutes(shift))) {
            LocalResult::Single(instant) => Some(instant.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
            LocalResult::None => None,
        }
    })
}
