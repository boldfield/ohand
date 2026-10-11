use chrono::{DateTime, Utc};

use crate::providers::contracts::ErrorClass;

/// A normalized failure of a native notification effect. Carries only the class and a fixed
/// code, never identifiers, content or the text of an underlying system error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationEffectError {
    pub class: ErrorClass,
    pub code: String,
}

impl NotificationEffectError {
    pub fn new(class: ErrorClass, code: impl Into<String>) -> NotificationEffectError {
        NotificationEffectError {
            class,
            code: code.into(),
        }
    }
}

impl NotificationEffectError {
    /// Whether the native layer refused the effect definitively. `permanent`, `unauthorized`
    /// and `unsupported` are surfaced and not retried automatically (m1-contracts.md,
    /// Normalized errors). `transient` is retried by the next reconciliation and `cancelled`
    /// proves nothing, so the request is re-examined against the pending set instead.
    pub fn is_definitive_rejection(&self) -> bool {
        is_definitive_rejection(self.class)
    }
}

pub fn is_definitive_rejection(class: ErrorClass) -> bool {
    match class {
        ErrorClass::Permanent | ErrorClass::Unauthorized | ErrorClass::Unsupported => true,
        ErrorClass::Transient | ErrorClass::Cancelled => false,
    }
}

impl std::fmt::Display for NotificationEffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}/{}", self.class, self.code)
    }
}

/// What a notification may say. Reconciliation only ever requests the generic payload;
/// item-specific previews are produced by other tasks, never here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    Generic,
}

impl PayloadKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PayloadKind::Generic => "generic",
        }
    }
}

/// Schedule request: fixed wording plus the opaque item identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRequest {
    pub notification_id: String,
    pub fire_at: DateTime<Utc>,
    pub payload_kind: PayloadKind,
    pub opaque_target_id: String,
}

/// One request the OS holds, keyed by the core-derived identifier. `fire_at` is `None` when
/// the OS could not report it, in which case the request is replaced rather than trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub notification_id: String,
    pub fire_at: Option<DateTime<Utc>>,
}

/// The injected native notification effect interface (m1-contracts.md, Notification Effect
/// Interface). Scheduling an identifier that is already pending replaces it; cancelling an
/// absent identifier succeeds.
pub trait NotificationEffects {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError>;
    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError>;
    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError>;
}
