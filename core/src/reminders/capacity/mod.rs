// OS pending-request capacity for native notifications (N04a).
//
// The OS keeps at most 64 pending local notification requests per app and, observed in
// docs/validation/notification-probe.md, accepts additions beyond that without an error and
// silently evicts the oldest-added ones. Core therefore never plans to the limit: it plans at
// most `OS_PENDING_LIMIT - REPLACEMENT_HEADROOM` (60) requests and makes every request that
// does not fit visible instead of letting the OS choose.
//
// Allocation rule (`allocate`, pure and deterministic):
// - Explicit reminders take slots before optional prompts. Prompts only receive what is left,
//   at most `MAX_PROMPT_REQUESTS` (7) and only within `PROMPT_HORIZON` (7 days) of now.
// - Reminders compete by due instant, soonest first; ties go to the earlier-created reminder,
//   then the lower reminder ID. The order depends on nothing but the facts, so a retry or a
//   refill pass repeats the same answer and a new sooner reminder displaces the farthest one,
//   which has the most time left to be refilled.
// - Requests the OS holds that are neither reminders nor prompt candidates cannot be reclaimed
//   and reduce the budget.
// - A reminder whose due instant has passed needs no slot. It stays `resolved` for N03, and
//   the passage of time is never recorded as delivery, attention or acknowledgment.
//
// Overflow is explicit, never dropped and never claimed installed:
// - A reminder without a slot becomes `unschedulable(capacity_exceeded)` with its instant and
//   schedule generation intact. It is no longer desired, so reconciliation cancels any request
//   the OS still holds for it. Nothing is installed for it.
// - Overflow prompts are returned as deferred with a reason, and their pending requests are
//   cancelled before reminders are installed.
//
// Refill rule: there is no timer. Every pass (after writes, on launch and on foreground
// activation) recomputes the allocation. When a slot frees because an earlier reminder was
// completed, cancelled, deleted or corrected, or its due time passed, the highest-priority
// held-back reminder returns to `resolved` with the same generation, hence the same native
// identifier, and the next reconciliation installs it. If its due time passes while held back
// it becomes `unschedulable(time_in_past)`, exactly like a reminder that was never installed.
// Missed prompts are not replayed. A schedule the OS rejected definitively stays rejected
// across a demotion and refill and is withheld until explicitly re-armed.

mod allocation;
mod apply;

#[cfg(test)]
mod tests;

pub use allocation::{
    allocate, Allocation, AllocationInput, CapacityPolicy, DeferredPrompt, PromptDeferral,
    PromptDemand, ReminderDemand, MAX_PROMPT_REQUESTS, OS_PENDING_LIMIT, PROMPT_HORIZON,
    REPLACEMENT_HEADROOM,
};
pub use apply::{reconcile_with_capacity, CapacityReport};
