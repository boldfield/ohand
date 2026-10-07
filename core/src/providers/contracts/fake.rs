//! Deterministic scripted provider for contract tests. It scripts only raw transport behavior
//! (delay, bytes, transport errors); all normalized outcomes come from `dispatch`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::dispatch::{AdapterCall, ManualClock, ProviderAdapter, TransportError};

#[derive(Debug, Clone)]
pub enum FakeAction {
    Respond(Vec<u8>),
    Fail(TransportError),
    /// Cancel the call's token mid-flight, then still return bytes.
    CancelThenRespond(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct FakeStep {
    pub delay_ms: u64,
    pub action: FakeAction,
}

impl FakeStep {
    pub fn respond(body: impl Into<Vec<u8>>) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::Respond(body.into()),
        }
    }

    pub fn fail(error: TransportError) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::Fail(error),
        }
    }

    pub fn cancel_then_respond(body: impl Into<Vec<u8>>) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::CancelThenRespond(body.into()),
        }
    }

    pub fn after_ms(mut self, delay_ms: u64) -> FakeStep {
        self.delay_ms = delay_ms;
        self
    }
}

/// What the fake observed for one call (the exact serialized request it was handed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    pub wire_request: String,
    pub deadline_ms: u64,
    pub max_response_bytes: usize,
}

pub struct FakeProvider {
    clock: Arc<ManualClock>,
    script: Mutex<VecDeque<FakeStep>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakeProvider {
    /// `clock` must be the same clock passed to `dispatch`; scripted delays advance it.
    pub fn new(
        clock: Arc<ManualClock>,
        script: impl IntoIterator<Item = FakeStep>,
    ) -> FakeProvider {
        FakeProvider {
            clock,
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl ProviderAdapter for FakeProvider {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        self.calls.lock().expect("calls lock").push(RecordedCall {
            wire_request: serde_json::to_string(call.request).expect("request serializes"),
            deadline_ms: call.deadline_ms,
            max_response_bytes: call.max_response_bytes,
        });
        let step = self
            .script
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or(TransportError::Rejected)?;
        self.clock.advance_ms(step.delay_ms);
        match step.action {
            FakeAction::Respond(body) => Ok(body),
            FakeAction::Fail(error) => Err(error),
            FakeAction::CancelThenRespond(body) => {
                call.cancel.cancel();
                Ok(body)
            }
        }
    }
}
