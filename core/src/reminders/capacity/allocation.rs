use std::cmp::Ordering;
use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};

/// The OS pending-request limit observed for local notifications (notification-probe.md:
/// 64 per app; additions beyond it report no error and silently evict).
pub const OS_PENDING_LIMIT: usize = 64;

/// Slots kept free so a replacement can be installed while the request it replaces, or a
/// request whose cancel is still outstanding, is still pending. The allocator never plans
/// above `OS_PENDING_LIMIT - REPLACEMENT_HEADROOM`.
pub const REPLACEMENT_HEADROOM: usize = 4;

/// How far ahead optional prompts may be planned.
pub const PROMPT_HORIZON: Duration = Duration::days(7);

/// The most optional prompt requests that may be pending at once.
pub const MAX_PROMPT_REQUESTS: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacityPolicy {
    pub os_pending_limit: usize,
    pub replacement_headroom: usize,
    pub prompt_horizon: Duration,
    pub max_prompt_requests: usize,
}

impl Default for CapacityPolicy {
    fn default() -> CapacityPolicy {
        CapacityPolicy {
            os_pending_limit: OS_PENDING_LIMIT,
            replacement_headroom: REPLACEMENT_HEADROOM,
            prompt_horizon: PROMPT_HORIZON,
            max_prompt_requests: MAX_PROMPT_REQUESTS,
        }
    }
}

impl CapacityPolicy {
    /// Pending requests the allocator may plan, always below the OS limit.
    pub fn usable_budget(&self) -> usize {
        self.os_pending_limit
            .saturating_sub(self.replacement_headroom.max(1))
    }
}

/// An explicit reminder that wants an OS slot: resolved, or held back earlier for capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderDemand {
    pub reminder_id: String,
    pub fire_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// An optional prompt candidate. The identifier is supplied by the prompt scheduler and is the
/// request identifier used verbatim with the OS, so it is stable across retries and refills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptDemand {
    pub notification_id: String,
    pub fire_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct AllocationInput<'a> {
    pub now: DateTime<Utc>,
    /// Pending OS requests that are neither explicit reminders nor prompt candidates. Core
    /// cannot reclaim them, so they reduce the budget of both.
    pub foreign_pending: usize,
    pub reminders: &'a [ReminderDemand],
    pub prompts: &'a [PromptDemand],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptDeferral {
    /// The planned time has passed. Missed prompts are not replayed.
    Elapsed,
    /// Farther ahead than the horizon; considered again on a later pass.
    BeyondHorizon,
    /// Within the horizon but past the prompt request limit.
    PromptLimit,
    /// Explicit reminders and earlier prompts left no slot.
    CapacityExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredPrompt {
    pub notification_id: String,
    pub reason: PromptDeferral,
}

/// Every demand appears in exactly one list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allocation {
    /// Slots available to explicit reminders after foreign requests.
    pub reminder_budget: usize,
    /// Reminders that hold an OS slot, in priority order.
    pub granted_reminders: Vec<String>,
    /// Reminders without a slot (`capacity_exceeded`), in priority order. Each is refilled by
    /// a later pass when a slot frees.
    pub overflow_reminders: Vec<String>,
    /// Reminders whose due time is at or before `now`. They need no slot and are left to
    /// reconciliation; the passage of time says nothing about delivery.
    pub elapsed_reminders: Vec<String>,
    pub granted_prompts: Vec<String>,
    pub deferred_prompts: Vec<DeferredPrompt>,
}

fn reminder_priority(left: &ReminderDemand, right: &ReminderDemand) -> Ordering {
    (left.fire_at, left.created_at, &left.reminder_id).cmp(&(
        right.fire_at,
        right.created_at,
        &right.reminder_id,
    ))
}

/// Pure and deterministic: the result depends only on the policy and input, never on what is
/// installed, so a retry or a refill pass reaches the same answer from the same facts.
///
/// Explicit reminders take slots first, soonest due first (ties by creation time, then
/// identifier). Optional prompts only receive what the reminders leave.
pub fn allocate(policy: &CapacityPolicy, input: &AllocationInput<'_>) -> Allocation {
    let reminder_budget = policy.usable_budget().saturating_sub(input.foreign_pending);

    let mut upcoming: Vec<&ReminderDemand> = Vec::new();
    let mut allocation = Allocation {
        reminder_budget,
        ..Allocation::default()
    };
    for demand in input.reminders {
        if demand.fire_at <= input.now {
            allocation
                .elapsed_reminders
                .push(demand.reminder_id.clone());
        } else {
            upcoming.push(demand);
        }
    }
    upcoming.sort_by(|left, right| reminder_priority(left, right));
    for (position, demand) in upcoming.iter().enumerate() {
        if position < reminder_budget {
            allocation
                .granted_reminders
                .push(demand.reminder_id.clone());
        } else {
            allocation
                .overflow_reminders
                .push(demand.reminder_id.clone());
        }
    }

    let mut prompts: Vec<&PromptDemand> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for demand in input.prompts {
        if !seen.insert(demand.notification_id.as_str()) {
            continue;
        }
        if demand.fire_at <= input.now {
            allocation.deferred_prompts.push(DeferredPrompt {
                notification_id: demand.notification_id.clone(),
                reason: PromptDeferral::Elapsed,
            });
        } else if demand.fire_at > input.now + policy.prompt_horizon {
            allocation.deferred_prompts.push(DeferredPrompt {
                notification_id: demand.notification_id.clone(),
                reason: PromptDeferral::BeyondHorizon,
            });
        } else {
            prompts.push(demand);
        }
    }
    prompts.sort_by(|left, right| {
        (left.fire_at, &left.notification_id).cmp(&(right.fire_at, &right.notification_id))
    });
    let prompt_slots = reminder_budget.saturating_sub(allocation.granted_reminders.len());
    for (position, demand) in prompts.iter().enumerate() {
        if position >= policy.max_prompt_requests {
            allocation.deferred_prompts.push(DeferredPrompt {
                notification_id: demand.notification_id.clone(),
                reason: PromptDeferral::PromptLimit,
            });
        } else if position >= prompt_slots {
            allocation.deferred_prompts.push(DeferredPrompt {
                notification_id: demand.notification_id.clone(),
                reason: PromptDeferral::CapacityExceeded,
            });
        } else {
            allocation
                .granted_prompts
                .push(demand.notification_id.clone());
        }
    }
    allocation
}
