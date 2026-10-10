//! The executor's record inside `Attempt::provider_output`.
//!
//! The XB01 attempt record has no room for arm order, allowance, rerun provenance or the
//! request fingerprint, so the executor stores them as one versioned envelope. Every field is a
//! core-assigned identifier, a counter or a fixed class string; model text appears only in
//! `output`, which the journal guard scans like any other provider output.

use ohand_core::providers::contracts::{DiagnosticMetadata, UsageAvailability};
use serde::{Deserialize, Serialize};

use super::error::ExecutorError;

pub(super) const ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    /// Written before dispatch. Without a terminal record the outcome is unknown.
    Reserved,
    Completed,
    Failed,
    /// Nothing was sent.
    Skipped,
}

pub(super) mod class {
    pub const TIMED_OUT: &str = "timed_out";
    pub const CANCELLED: &str = "cancelled";
    pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";
    pub const NOT_AUTHORIZED: &str = "not_authorized";
    pub const NOT_ELIGIBLE: &str = "not_eligible";
    pub const OUTPUT_REJECTED: &str = "output_rejected";
    pub const UNSUPPORTED: &str = "unsupported";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Envelope {
    pub executor_record: u32,
    pub phase: Phase,
    /// True once a request was reserved for dispatch.
    pub dispatched: bool,
    /// Which run of this experiment wrote the record (1-based).
    pub run: u32,
    /// Cumulative reservation number across all runs of the experiment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<u32>,
    /// Attempt number within this side's retry sequence (1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_attempt: Option<u32>,
    /// The recorded arm order for the case, first arm first.
    pub arm_order: Vec<String>,
    /// Digest of both arms' derived requests; `None` when no pair could be derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<DiagnosticMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageAvailability>,
}

impl Envelope {
    pub fn new(phase: Phase, dispatched: bool, run: u32, arm_order: Vec<String>) -> Envelope {
        Envelope {
            executor_record: ENVELOPE_VERSION,
            phase,
            dispatched,
            run,
            reservation: None,
            side_attempt: None,
            arm_order,
            pair_fingerprint: None,
            retry_of: None,
            rerun_of: None,
            class: None,
            detail: None,
            output: None,
            metadata: None,
            usage: None,
        }
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("envelope fields always serialize")
    }

    pub fn from_value(value: Option<&serde_json::Value>) -> Result<Envelope, ExecutorError> {
        let envelope: Envelope = value
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .ok_or(ExecutorError::ForeignRecord)?;
        if envelope.executor_record != ENVELOPE_VERSION {
            return Err(ExecutorError::ForeignRecord);
        }
        Ok(envelope)
    }
}
