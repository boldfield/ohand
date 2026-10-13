// Bounded optional daily prompt schedule (S02).
//
// One generic local prompt per day, only after the user has chosen a time. Off by default.
//
// - Horizon: the next `PROMPT_HORIZON_DAYS` (3) occurrences, below the capacity allocator's own
//   limits (7 requests, 7 days). Explicit reminders are allocated first (N04), so prompts only
//   ever use slots reminders leave free, and a prompt that does not fit is deferred, never
//   installed.
// - Refill: only on app open, or when the user changes the setting in the open app. Re-entry
//   and clock or zone changes drop passed occurrences and add none. A missed prompt is never
//   replayed, and after several days of nonuse the OS holds no prompt: no backlog or catch-up.
// - Ledger, not plan state: whether a local date has had its prompt is recorded in
//   `PromptSchedule` apart from the plan, as an append-only set of spent dates plus the
//   high-water mark of every clock reading seen. Every entry point that takes `now`
//   (`configure`, `refresh`, `record_capacity_pass`, `reconcile_prompts`, `cancel_prompts`)
//   settles that ledger first, with the clock before any fallible work (the OS listing) and
//   only then rebuilds, cancels or discards anything, so no ordering of time changes, app
//   opens, disable/re-enable, capacity passes, failed listings and backward clock changes can
//   lose the evidence that a prompt fired or that a clock reading was lived through. A date is spent once its request was
//   attempted (recorded as held before the native call, so a lost acknowledgment still counts)
//   and the clock has reached its instant, or an OS listing no longer shows it although core
//   did not cancel it, or core discards it without a listing (disable). A request core cancels
//   after seeing it pending in the same pass (capacity, or a changed plan) has not fired and
//   comes back when a slot frees; so does one whose install the OS definitively rejected.
//   Ambiguity fails toward suppression: a prompt whose delivery is uncertain is skipped, never
//   replayed, and an ambiguous install failure that in fact installed nothing costs that date.
//   Spent dates are pruned only by age (`SPENT_DATE_RETENTION_DAYS`); a rollback larger than
//   that is out of scope.
// - Error paths: provenance core already holds is recorded before the next fallible call. A
//   pass releases the prompts it cancelled outside the plan before the capacity pass lists the
//   OS again, so a failure there cannot turn core's own cancellation into an apparent delivery
//   that skips a still-future day. A native effect whose evidence a returned error does lose
//   (a cancel the capacity pass made before failing) can cost that date its prompt: a skip
//   after an error is suppression, not a defect; only replay is.
// - One prompt per local date: the identifier is `prompt:<local date>`, and once a date is
//   spent no time on that date is planned again, even if the user moves the time later.
// - Disable: cancels every pending prompt and nothing else. No capacity pass runs, so reminders
//   keep their state and their OS requests. The dates of the cancelled prompts are spent, so
//   re-enabling plans from the next unspent date.
// - Payload: always `PayloadKind::Generic`; the opaque target is the prompt's own identifier
//   (`prompt:<local date>`). No item text, count or route is available to this module.
// - Persistence: `PromptSchedule` is a serializable value held by the caller with its other
//   configuration, ledger included. This module owns no table, so the caller must persist the
//   schedule after every call that takes `now` (`configure`, `refresh`, `record_capacity_pass`,
//   `reconcile_prompts`, `cancel_prompts`), and before it acts on the pass report. Known limit:
//   a crash between a pass that installed a prompt and that persist loses the pass's held and
//   spent evidence; if the clock is also rolled back across that window before the next
//   observation, the prompt installed in it can be replayed. Narrowing that window further
//   needs a durable attempt marker owned outside this module.

mod apply;
mod plan;

pub use apply::{
    cancel_prompts, reconcile_prompts, PromptFailure, PromptOperation, PromptPassReport,
};
pub use plan::{
    is_prompt_notification_id, local_fire_instant, prompt_local_date, prompt_notification_id,
    PlannedPrompt, PromptSchedule, PromptScheduleError, PromptSettings, PromptTime, RefreshTrigger,
    PROMPT_HORIZON_DAYS, PROMPT_ID_PREFIX, SPENT_DATE_RETENTION_DAYS,
};
