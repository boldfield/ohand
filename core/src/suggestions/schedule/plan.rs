use std::collections::{BTreeMap, BTreeSet};

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

/// How long a spent local date stays in the ledger, measured from the high-water date. A clock
/// rolled back further than this can replay a prompt that fired before the rollback; a larger
/// rollback is out of scope.
pub const SPENT_DATE_RETENTION_DAYS: i64 = 14;

const _: () = assert!(PROMPT_HORIZON_DAYS <= MAX_PROMPT_REQUESTS);
const _: () = assert!(
    Duration::days(PROMPT_HORIZON_DAYS as i64 + 1).num_seconds() <= PROMPT_HORIZON.num_seconds()
);

/// Longest DST gap searched for the first valid local minute after a skipped time.
const MAX_GAP_MINUTES: i64 = 180;

/// Most dates a fill scans forward before giving up on a full horizon.
const FILL_SCAN_LIMIT_DAYS: usize = 366;

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

/// One planned local-date occurrence. Plan entries carry no delivery evidence: that lives in
/// the schedule's ledger, which survives every rebuild of the plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedPrompt {
    pub notification_id: String,
    pub local_date: NaiveDate,
    pub fire_at: DateTime<Utc>,
}

/// The bounded optional daily prompt schedule: the user's setting, the occurrences planned
/// from it, and the ledger that says which local dates have had their prompt. A plain value.
/// The caller keeps it with the app's other configuration (it serializes to JSON) and hands it
/// to `reconcile_prompts` to bring the OS in line.
///
/// Rules:
/// - Off by default; a daily prompt exists only after a local time has been chosen.
/// - At most [`PROMPT_HORIZON_DAYS`] occurrences are planned, always ahead of every clock
///   reading seen so far. The horizon is refilled only by [`RefreshTrigger::AppOpen`] or by the
///   user changing the setting, which happens inside the open app. Re-entry and clock or zone
///   changes only drop occurrences; they never add one.
/// - A passed occurrence is never replayed or made up, so nonuse leaves nothing queued and the
///   next open starts from the next future occurrence.
///
/// The ledger, kept apart from the plan:
/// - `high_water` is the latest `now` any entry point has observed. The plan never holds an
///   instant at or before it, so a backward clock change cannot plan a date the user has
///   already lived through, whatever the plan looked like when the clock moved.
/// - `held` records, per local date, the instant of the request core handed to the OS and has
///   not since cancelled. It is the only evidence that a prompt may have been delivered.
/// - `spent` is the append-only set of local dates whose prompt has been delivered, or may have
///   been. No date is planned again while it is in the set. A date is spent when it is held and
///   (a) the high-water mark has reached its instant, or (b) an OS listing no longer shows its
///   request although core did not cancel it, or (c) core cancels it or empties the plan
///   containing it without having seen it still pending in that same pass, which is what
///   disabling does. A request core cancels after seeing it pending in the same pass, for
///   capacity or because the plan changed, has not fired: it is merely released, and comes
///   back when a slot frees or the plan holds it again. Ambiguity fails toward suppression:
///   skipping a prompt whose delivery is uncertain is intended; replaying one is the defect.
/// - Nothing removes a date from `spent` except age: dates more than
///   [`SPENT_DATE_RETENTION_DAYS`] before the high-water date are pruned.
/// - Every entry point that takes `now` first raises the high-water mark and settles the
///   ledger ([`PromptSchedule::observe`]), and only then rebuilds, cancels or discards anything.
///
/// Also:
/// - The identifier of an occurrence is `prompt:<local date>`: stable, so a re-plan replaces
///   rather than duplicates, and it carries no item content. Each local date has at most one
///   prompt.
/// - Local-time semantics: a skipped local time (DST gap) fires at the first valid minute after
///   the gap; a repeated local time (DST fold) fires at its first occurrence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSchedule {
    settings: PromptSettings,
    planned: Vec<PlannedPrompt>,
    /// The latest clock reading any entry point has observed.
    #[serde(default)]
    high_water: Option<DateTime<Utc>>,
    /// Local dates whose request core handed to the OS and has not cancelled since, with the
    /// instant of that request. Independent of the plan.
    #[serde(default)]
    held: BTreeMap<NaiveDate, DateTime<Utc>>,
    /// Local dates that have had their prompt, or may have. Append-only apart from age pruning.
    #[serde(default)]
    spent: BTreeSet<NaiveDate>,
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

    /// The latest clock reading this schedule has observed, if any.
    pub fn high_water(&self) -> Option<DateTime<Utc>> {
        self.high_water
    }

    /// Whether `local_date` has had its prompt (or may have) and is therefore never planned
    /// again while it stays in the ledger.
    pub fn is_spent(&self, local_date: NaiveDate) -> bool {
        self.spent.contains(&local_date)
    }

    /// Apply the user's choice. Off empties the plan, and every date whose request core had
    /// handed to the OS is spent: nothing has shown those requests still pending, so turning
    /// prompting back on never plans them again. A time rebuilds the horizon ahead of the
    /// high-water mark and the ledger, so an earlier time today that has already passed is not
    /// replayed, and a date whose request the OS fired while the clock was moved back is spent
    /// by the next pass however often the time changes in between.
    pub fn configure(&mut self, settings: PromptSettings, now: DateTime<Utc>, zone: Tz) {
        self.observe(now);
        self.settings = settings;
        if !self.is_enabled() {
            self.spend_held_without_listing();
        }
        self.rebuild(zone);
    }

    pub fn refresh(&mut self, trigger: RefreshTrigger, now: DateTime<Utc>, zone: Tz) {
        self.observe(now);
        if !self.is_enabled() {
            self.spend_held_without_listing();
            self.planned.clear();
            return;
        }
        if trigger == RefreshTrigger::AppOpen {
            self.rebuild(zone);
        }
    }

    /// The chokepoint. Raises the high-water mark to `now`, spends every held date whose
    /// instant the high-water mark has reached, prunes the ledger by age, and drops planned
    /// occurrences at or before the high-water mark or on a spent date. Every entry point calls
    /// this before it rebuilds, cancels or discards anything.
    pub fn observe(&mut self, now: DateTime<Utc>) {
        let high_water = self.high_water.map_or(now, |previous| previous.max(now));
        self.high_water = Some(high_water);
        let reached: Vec<NaiveDate> = self
            .held
            .iter()
            .filter(|(_, fire_at)| **fire_at <= high_water)
            .map(|(local_date, _)| *local_date)
            .collect();
        self.spend(reached);
        let oldest_kept = high_water.date_naive() - Duration::days(SPENT_DATE_RETENTION_DAYS);
        self.spent.retain(|local_date| *local_date >= oldest_kept);
        self.settle_plan();
    }

    /// The chokepoint with an OS listing in hand: after [`observe`](Self::observe), every held
    /// date whose request the listing no longer shows is spent, because core did not cancel it
    /// (a cancel releases the date first). Called by the reconcile pass before it allocates or
    /// cancels anything, with the identifiers the OS currently holds.
    pub(super) fn observe_pending(&mut self, now: DateTime<Utc>, pending: &BTreeSet<&str>) {
        self.observe(now);
        let vanished: Vec<NaiveDate> = self
            .held
            .keys()
            .copied()
            .filter(|local_date| !pending.contains(prompt_notification_id(*local_date).as_str()))
            .collect();
        self.spend(vanished);
        self.settle_plan();
    }

    /// Core handed the OS a request for `local_date` at `fire_at`, or saw the OS holding one.
    pub(super) fn record_held(&mut self, local_date: NaiveDate, fire_at: DateTime<Utc>) {
        if !self.spent.contains(&local_date) {
            self.held.insert(local_date, fire_at);
        }
    }

    /// Core cancelled these requests after seeing them pending in the same pass. The OS had
    /// not fired them, so their dates are released, not spent, and are planned again when a slot
    /// frees or the plan holds them.
    pub(super) fn release<'a>(&mut self, cancelled: impl IntoIterator<Item = &'a str>) {
        for notification_id in cancelled {
            if let Some(local_date) = prompt_local_date(notification_id) {
                self.held.remove(&local_date);
            }
        }
    }

    /// Record a capacity pass that ran outside [`reconcile_prompts`](super::reconcile_prompts)
    /// with this schedule's [`demands`](Self::demands), such as the reminder side making room
    /// for a new reminder, at the instant `now` that pass used. The clock reading settles the
    /// ledger first, so a prompt whose instant has passed by then is spent even though the
    /// demands no longer named it. The prompts the pass cancelled were pending in its own
    /// listing, so they are released, never spent, and come back when a slot frees. Every
    /// caller that offers these demands to a capacity pass must hand its report back here; an
    /// unreported cancellation is indistinguishable from a delivery and spends the date.
    pub fn record_capacity_pass(&mut self, report: &CapacityReport, now: DateTime<Utc>) {
        self.observe(now);
        self.release(report.prompts_cancelled.iter().map(String::as_str));
    }

    /// The prompt candidates to offer the capacity pass: none unless enabled, only future
    /// occurrences on unspent dates, soonest first and never more than the horizon.
    pub fn demands(&self, now: DateTime<Utc>) -> Vec<PromptDemand> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let mut upcoming: Vec<&PlannedPrompt> = self
            .planned
            .iter()
            .filter(|prompt| {
                prompt.fire_at > now
                    && !self.spent.contains(&prompt.local_date)
                    && prompt.notification_id.starts_with(PROMPT_ID_PREFIX)
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

    /// Replace the plan with a fresh fill ahead of the high-water mark and the ledger.
    fn rebuild(&mut self, zone: Tz) {
        let Some(floor) = self.high_water else {
            self.planned.clear();
            return;
        };
        self.planned = fill(self.settings, floor, zone, &self.spent);
    }

    /// Case (c): the plan is being emptied without an OS listing, so nothing has shown the held
    /// requests still pending. Every held date is spent.
    fn spend_held_without_listing(&mut self) {
        let held: Vec<NaiveDate> = self.held.keys().copied().collect();
        self.spend(held);
    }

    fn spend(&mut self, local_dates: Vec<NaiveDate>) {
        for local_date in local_dates {
            self.held.remove(&local_date);
            self.spent.insert(local_date);
        }
    }

    /// Drop planned occurrences the ledger rules out: at or before the high-water mark, or on
    /// a spent date.
    fn settle_plan(&mut self) {
        let high_water = self.high_water;
        let spent = &self.spent;
        self.planned.retain(|prompt| {
            high_water.is_none_or(|mark| prompt.fire_at > mark)
                && !spent.contains(&prompt.local_date)
        });
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

/// The local date a prompt identifier names, if it is one.
pub fn prompt_local_date(notification_id: &str) -> Option<NaiveDate> {
    if !is_prompt_notification_id(notification_id) {
        return None;
    }
    notification_id.strip_prefix(PROMPT_ID_PREFIX)?.parse().ok()
}

fn fill(
    settings: PromptSettings,
    floor: DateTime<Utc>,
    zone: Tz,
    spent: &BTreeSet<NaiveDate>,
) -> Vec<PlannedPrompt> {
    let PromptSettings::Daily { time } = settings else {
        return Vec::new();
    };
    let mut planned = Vec::with_capacity(PROMPT_HORIZON_DAYS);
    let mut local_date = floor.with_timezone(&zone).date_naive();
    // Today's occurrence may already be past, spent dates are skipped (and repeated
    // disable/re-enable cycles can spend a run of future dates), and a gap can push one past
    // the floor again, so scan forward until the horizon is full rather than assuming the
    // first `PROMPT_HORIZON_DAYS` dates qualify. The ledger is finite, so this terminates;
    // the hard bound only guards against a pathological ledger.
    for _ in 0..FILL_SCAN_LIMIT_DAYS {
        if planned.len() == PROMPT_HORIZON_DAYS {
            break;
        }
        if let Some(fire_at) = local_fire_instant(local_date, time, zone) {
            if fire_at > floor && !spent.contains(&local_date) {
                planned.push(PlannedPrompt {
                    notification_id: prompt_notification_id(local_date),
                    local_date,
                    fire_at,
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
