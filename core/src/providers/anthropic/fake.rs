//! Deterministic scripted Anthropic HTTP transport for protocol fixtures. It scripts only what
//! a native transport can do (return an HTTP response, time out, observe cancellation); every
//! mapping to shared outcomes is done by the adapter and `dispatch`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{AnthropicTransport, HttpRequest, HttpResponse};
use crate::providers::contracts::{CancelToken, Clock, ManualClock, TransportError};

#[derive(Debug, Clone)]
pub enum FakeAnthropicAction {
    Respond(HttpResponse),
    Fail(TransportError),
}

#[derive(Debug, Clone)]
pub struct FakeAnthropicStep {
    pub action: FakeAnthropicAction,
    pub delay_ms: u64,
    pub cancel_during_call: bool,
    pub honors_cancellation: bool,
    pub enforces_deadline: bool,
}

impl FakeAnthropicStep {
    fn new(action: FakeAnthropicAction) -> Self {
        FakeAnthropicStep {
            action,
            delay_ms: 0,
            cancel_during_call: false,
            honors_cancellation: true,
            enforces_deadline: true,
        }
    }

    pub fn respond(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::new(FakeAnthropicAction::Respond(HttpResponse {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.into(),
        }))
    }

    pub fn respond_json(status: u16, body: &serde_json::Value) -> Self {
        Self::respond(
            status,
            serde_json::to_vec(body).expect("fixture serializes"),
        )
    }

    pub fn fail(error: TransportError) -> Self {
        Self::new(FakeAnthropicAction::Fail(error))
    }

    /// Advance the shared clock by `delay_ms` before answering.
    pub fn after_ms(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// Cancel the call's token while the request is in flight.
    pub fn cancelling_in_flight(mut self) -> Self {
        self.cancel_during_call = true;
        self
    }

    /// Model a transport that does not notice cancellation and still returns its answer.
    pub fn ignoring_cancellation(mut self) -> Self {
        self.honors_cancellation = false;
        self
    }

    /// Model a transport that does not enforce the request timeout.
    pub fn ignoring_deadline(mut self) -> Self {
        self.enforces_deadline = false;
        self
    }
}

pub struct FakeAnthropicTransport {
    clock: Arc<ManualClock>,
    script: Mutex<VecDeque<FakeAnthropicStep>>,
    calls: Mutex<Vec<HttpRequest>>,
}

impl FakeAnthropicTransport {
    /// `clock` must be the clock passed to `dispatch`; scripted delays advance it.
    pub fn new(
        clock: Arc<ManualClock>,
        script: impl IntoIterator<Item = FakeAnthropicStep>,
    ) -> Self {
        FakeAnthropicTransport {
            clock,
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Every request handed to the transport, in order.
    pub fn calls(&self) -> Vec<HttpRequest> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl AnthropicTransport for FakeAnthropicTransport {
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        self.calls.lock().expect("calls lock").push(request.clone());
        let step = self
            .script
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or(TransportError::Unavailable)?;
        self.clock.advance_ms(step.delay_ms);
        if step.cancel_during_call {
            cancel.cancel();
        }
        if step.honors_cancellation && cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if step.enforces_deadline && self.clock.now_ms() > request.deadline_ms {
            return Err(TransportError::Timeout);
        }
        match step.action {
            FakeAnthropicAction::Respond(response) => Ok(response),
            FakeAnthropicAction::Fail(error) => Err(error),
        }
    }
}
