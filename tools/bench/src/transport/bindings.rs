//! Thin bindings of [`HostTransport`] to the provider transport traits in core. They translate
//! shapes only; destination, credential, bound and deadline enforcement live in the host effect.

use std::time::Duration;

use ohand_core::providers::anthropic::{AnthropicTransport, HttpRequest, HttpResponse};
use ohand_core::providers::contracts::{CancelToken, Clock, TransportError};
use ohand_core::providers::openai::HttpTransport;

use super::host::{AttachmentRule, ClockDeadline, CredentialUse, HostTransport, SecureRequest};

impl AnthropicTransport for HostTransport {
    /// The request carries a remaining-time budget; its absolute dispatch-clock deadline needs
    /// a clock this trait does not supply, so the relative budget is the enforced bound.
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let secure_request = SecureRequest {
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            timeout: Duration::from_millis(request.timeout_ms),
            max_response_bytes: request.max_response_bytes,
            credential: request.credential.as_ref().map(|attachment| CredentialUse {
                reference: attachment.reference.clone(),
                rule: AttachmentRule {
                    header: attachment.header.clone(),
                    scheme: None,
                },
            }),
        };
        let response = HostTransport::send(self, &secure_request, cancel, None)?;
        Ok(HttpResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }
}

impl HttpTransport for HostTransport {
    /// Attaches the secret behind `credential_ref` as `Authorization: Bearer <secret>`.
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &CancelToken,
        clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let remaining_ms = deadline_ms.saturating_sub(clock.now_ms());
        let secure_request = SecureRequest {
            url: endpoint.to_string(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            body,
            timeout: Duration::from_millis(remaining_ms),
            max_response_bytes: usize::try_from(max_response_bytes).unwrap_or(usize::MAX - 1),
            credential: Some(CredentialUse {
                reference: credential_ref.to_string(),
                rule: AttachmentRule {
                    header: "authorization".to_string(),
                    scheme: Some("Bearer".to_string()),
                },
            }),
        };
        let response = HostTransport::send(
            self,
            &secure_request,
            cancel,
            Some(ClockDeadline { clock, deadline_ms }),
        )?;
        Ok((response.status, response.body))
    }
}
