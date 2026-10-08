//! Recording fake Anthropic transport for contract tests.

use super::AnthropicTransport;
use crate::providers::contracts::TransportError;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct FakeAnthropicStep {
    pub delay_ms: u64,
    pub action: FakeAnthropicAction,
}

#[derive(Debug, Clone)]
pub enum FakeAnthropicAction {
    Respond(Vec<u8>),
    Fail(TransportError),
}

impl FakeAnthropicStep {
    pub fn respond(body: impl Into<Vec<u8>>) -> Self {
        FakeAnthropicStep {
            delay_ms: 0,
            action: FakeAnthropicAction::Respond(body.into()),
        }
    }

    pub fn fail(error: TransportError) -> Self {
        FakeAnthropicStep {
            delay_ms: 0,
            action: FakeAnthropicAction::Fail(error),
        }
    }

    pub fn after_ms(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
}

/// Recorded details of one Anthropic API call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAnthropicCall {
    pub endpoint: String,
    pub api_key_ref: String,
    pub body: Vec<u8>,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
}

pub struct FakeAnthropicTransportInner {
    script: Mutex<VecDeque<FakeAnthropicStep>>,
    calls: Mutex<Vec<RecordedAnthropicCall>>,
}

pub struct FakeAnthropicTransport {
    inner: Arc<FakeAnthropicTransportInner>,
}

impl Clone for FakeAnthropicTransport {
    fn clone(&self) -> Self {
        FakeAnthropicTransport {
            inner: self.inner.clone(),
        }
    }
}

impl FakeAnthropicTransport {
    pub fn new(script: impl IntoIterator<Item = FakeAnthropicStep>) -> Self {
        FakeAnthropicTransport {
            inner: Arc::new(FakeAnthropicTransportInner {
                script: Mutex::new(script.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn calls(&self) -> Vec<RecordedAnthropicCall> {
        self.inner.calls.lock().expect("calls lock").clone()
    }
}

impl AnthropicTransport for FakeAnthropicTransport {
    fn post(
        &self,
        endpoint: &str,
        api_key_ref: &str,
        body: &[u8],
        timeout_ms: u64,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, TransportError> {
        self.inner
            .calls
            .lock()
            .expect("calls lock")
            .push(RecordedAnthropicCall {
                endpoint: endpoint.to_string(),
                api_key_ref: api_key_ref.to_string(),
                body: body.to_vec(),
                timeout_ms,
                max_response_bytes,
            });

        let step = self
            .inner
            .script
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or(TransportError::Rejected)?;

        match step.action {
            FakeAnthropicAction::Respond(body) => Ok(body),
            FakeAnthropicAction::Fail(error) => Err(error),
        }
    }
}
