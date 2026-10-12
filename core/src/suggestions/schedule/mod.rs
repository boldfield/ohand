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
// - Disable: cancels every pending prompt and nothing else. No capacity pass runs, so reminders
//   keep their state and their OS requests.
// - Payload: always `PayloadKind::Generic`; the opaque target is the prompt's own identifier
//   (`prompt:<local date>`). No item text, count or route is available to this module.
// - Persistence: `PromptSchedule` is a serializable value held by the caller with its other
//   configuration. This module owns no table.

mod apply;
mod plan;

pub use apply::{
    cancel_prompts, reconcile_prompts, PromptFailure, PromptOperation, PromptPassReport,
};
pub use plan::{
    is_prompt_notification_id, local_fire_instant, prompt_notification_id, PlannedPrompt,
    PromptSchedule, PromptScheduleError, PromptSettings, PromptTime, RefreshTrigger,
    PROMPT_HORIZON_DAYS, PROMPT_ID_PREFIX,
};
