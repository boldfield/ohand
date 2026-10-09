//! Deterministic scripted provider for contract tests. It scripts only raw transport behavior
//! (delay, bytes, transport errors); all normalized outcomes come from `dispatch`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::diagnostic::{
    CallObservations, DiagnosticAdapterCall, DiagnosticResponse, DiagnosticSupport, SettingValue,
};
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
    /// What a diagnostic call discloses alongside the body; `None` models a provider that
    /// reports nothing. Ignored by ordinary calls.
    pub observations: Option<CallObservations>,
}

impl FakeStep {
    pub fn respond(body: impl Into<Vec<u8>>) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::Respond(body.into()),
            observations: None,
        }
    }

    pub fn fail(error: TransportError) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::Fail(error),
            observations: None,
        }
    }

    pub fn cancel_then_respond(body: impl Into<Vec<u8>>) -> FakeStep {
        FakeStep {
            delay_ms: 0,
            action: FakeAction::CancelThenRespond(body.into()),
            observations: None,
        }
    }

    pub fn with_observations(mut self, observations: CallObservations) -> FakeStep {
        self.observations = Some(observations);
        self
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

/// What the fake observed for one diagnostic call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDiagnosticCall {
    pub instructions: String,
    pub context: String,
    pub requested_settings: Vec<SettingValue>,
    pub wire_request: String,
}

pub struct FakeProvider {
    clock: Arc<ManualClock>,
    script: Mutex<VecDeque<FakeStep>>,
    calls: Mutex<Vec<RecordedCall>>,
    diagnostic_support: DiagnosticSupport,
    diagnostic_calls: Mutex<Vec<RecordedDiagnosticCall>>,
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
            diagnostic_support: DiagnosticSupport::unsupported(),
            diagnostic_calls: Mutex::new(Vec::new()),
        }
    }

    /// Opt this fake in to diagnostic calls; by default it behaves like a pre-diagnostic adapter.
    pub fn with_diagnostic_support(mut self, support: DiagnosticSupport) -> FakeProvider {
        self.diagnostic_support = support;
        self
    }

    pub fn diagnostic_calls(&self) -> Vec<RecordedDiagnosticCall> {
        self.diagnostic_calls
            .lock()
            .expect("diagnostic calls lock")
            .clone()
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

    fn diagnostic_support(&self) -> DiagnosticSupport {
        self.diagnostic_support.clone()
    }

    fn invoke_diagnostic(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, TransportError> {
        self.diagnostic_calls
            .lock()
            .expect("diagnostic calls lock")
            .push(RecordedDiagnosticCall {
                instructions: call.request.instructions().to_string(),
                context: call.request.context().to_string(),
                requested_settings: call.request.settings().values(),
                wire_request: serde_json::to_string(call.request).expect("request serializes"),
            });
        let step = self
            .script
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or(TransportError::Rejected)?;
        self.clock.advance_ms(step.delay_ms);
        match step.action {
            FakeAction::Respond(body) => Ok(DiagnosticResponse {
                body,
                observations: step.observations,
            }),
            FakeAction::Fail(error) => Err(error),
            FakeAction::CancelThenRespond(body) => {
                call.cancel.cancel();
                Ok(DiagnosticResponse {
                    body,
                    observations: step.observations,
                })
            }
        }
    }
}
