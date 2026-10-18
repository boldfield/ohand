//! Self-hosted (OpenAI-compatible) chat adapter for interpretation requests.
//!
//! This adapter supports exactly the protocol observed on the Spark endpoint by V08a (worker
//! context, probe evidence [`WORKER_EVIDENCE_ID`]) and V08b (phone context, maintainer evidence
//! [`PHONE_EVIDENCE_ID`]); nothing is assumed beyond those artifacts:
//!
//! - The profile endpoint is the server's base URL, and the adapter posts to
//!   `<base>/chat/completions`. The request carries `model`, `messages`, `temperature: 0`,
//!   `max_tokens`, `stream: false` and a `json_object` `response_format`. The output contract travels in the prompt and
//!   the reply is validated locally against `output_schema()`: a strict `json_schema` made Ollama
//!   suppress optional proposals (see [`compatibility_boundary`]). A `null` in an optional
//!   position is treated as absent before validation.
//! - Transport is the shared [`HttpTransport`] seam, so the native layer keeps enforcing https,
//!   certificate validation, the profile's authorized origin and secret handling. The adapter
//!   never sees a credential and contacts only the profile's own endpoint: there is no default
//!   endpoint and no second destination, so a failure can never be retried against a cloud
//!   provider by this adapter.
//! - The endpoint was observed to answer without authentication, so the credential reference
//!   is still passed for the transport to attach, but a 401/403 is the only authentication
//!   signal and it is mapped to [`TransportError::Unauthorized`]. That the endpoint validates
//!   the credential at all is unverified.
//! - Off the home network the configured hostname resolves to a public forwarder that answers
//!   302 (also for POST) pointing at the tailnet name. A profile for phone use must therefore
//!   use the tailnet hostname as its base URL. A redirect status that reaches the adapter means
//!   the endpoint is not reachable from this network, so it is reported as unavailable and the
//!   capture stays queued.
//!
//! Model support is per model: only the model that answered this adapter's own request in the
//! smoke run [`SMOKE_EVIDENCE_ID`] is `Supported`; see [`text_interpretation_capability`].

use crate::interpretation::instructions::{output_schema, render_prompt};
use crate::providers::contracts::{
    AdapterCall, CapabilityMetadata, ProviderAdapter, ProviderCapability, ProviderProtocol,
    StructuredOutputMode, TransportError,
};
use crate::providers::openai::HttpTransport;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Evidence identifier of the V08a worker-context probe artifact.
pub const WORKER_EVIDENCE_ID: &str = "spark-probe-20261008T093909Z-22c47772";
/// Repository path of the sanitized V08a artifact.
pub const WORKER_EVIDENCE_ARTIFACT: &str =
    "docs/validation/evidence/spark-probe/spark-probe-20261008T093909Z-22c47772.json";
/// Probe build revision recorded by the V08a artifact.
pub const WORKER_PROBE_REVISION: &str = "22c47772849c77b3d3a5e2b03be87098690e738e";
/// Collection time recorded by the V08a artifact.
pub const WORKER_EVIDENCE_COLLECTED_AT: &str = "2026-10-08T09:39:09Z";

/// Evidence identifier of the adapter smoke run: this adapter's own request, built from the real
/// interpretation prompt and `output_schema()`, sent to the configured endpoint.
pub const SMOKE_EVIDENCE_ID: &str = "self-hosted-adapter-smoke-20261009T145738Z-e45f02b4";
/// Repository path of the sanitized smoke artifact.
pub const SMOKE_EVIDENCE_ARTIFACT: &str =
    "docs/validation/evidence/spark-adapter-smoke/self-hosted-adapter-smoke-20261009T145738Z-e45f02b4.json";
/// Adapter build revision recorded by the smoke artifact.
pub const SMOKE_ADAPTER_REVISION: &str = "e45f02b4cdec971bdd0e16cd162af60afe8a4a16";
/// Collection time recorded by the smoke artifact.
pub const SMOKE_EVIDENCE_COLLECTED_AT: &str = "2026-10-09T14:57:38Z";

/// Evidence identifier of the V08b phone-context artifact (revision 2).
pub const PHONE_EVIDENCE_ID: &str = "spark-phone-2026-10-08";
/// Repository path of the sanitized V08b artifact.
pub const PHONE_EVIDENCE_ARTIFACT: &str =
    "docs/validation/evidence/spark-phone/2026-10-08-phone-reachability.json";
/// Latest collection time among the phone-context attempts that inform the boundary.
pub const PHONE_EVIDENCE_COLLECTED_AT: &str = "2026-10-08T16:39:00Z";

/// The only model that received a structured chat request in the probe and passed it.
pub const CHAT_VERIFIED_MODEL: &str = "deepseek-flash-iq3:latest";
/// Every model identifier the endpoint listed, in the order returned by the probe.
pub const LISTED_MODELS: [&str; 6] = [
    "deepseek-flash-iq3:latest",
    "qwen3.8:27b",
    "qwen3.5:122b",
    "gpt-oss:120b",
    "gpt-oss:20b",
    "glm-5.3-flash",
];

/// Completion token bound sent with every request. The verified model spends part of it on a
/// separate `reasoning` field; at 1024 the smoke run's reminder capture ended with
/// `finish_reason` `length` and empty content, so the bound is a ceiling, not a typical size.
pub const MAX_COMPLETION_TOKENS: u32 = 4096;
/// Path appended to the profile's base URL.
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// The exact verified compatibility boundary, one statement per entry.
pub fn compatibility_boundary() -> &'static [&'static str] {
    &[
        "Verified: https with certificate validation; GET /models and POST /chat/completions answer 200 in OpenAI-compatible form (worker context).",
        "Verified (probe): a strict json_schema response_format returned content matching a two-key schema for deepseek-flash-iq3:latest only.",
        "Verified (adapter smoke, worker context, max_tokens 4096): the adapter's own request, carrying the interpretation output schema (optional properties included) as a strict json_schema, was answered 200 with an assistant message and finish_reason stop for three synthetic captures, and each reply mapped to a validated proposal. Calls took up to about 56 seconds; the smoke profile's timeout was 300 seconds.",
        "Observed failure at max_tokens 1024 (artifacts self-hosted-adapter-smoke-20261009T145407Z-d1243e1f and -20261009T145540Z-d1243e1f): the reminder capture ended with finish_reason length and empty content because the model's separate reasoning output used the whole budget; the adapter reports InvalidOutput (not retriable). A request that needs more than the bound is therefore not interpreted; the capture itself stays durable.",
        "Not verified: reminder_proposal and session_topic_proposal output (no smoke capture produced them), other finish_reason values, streaming, rate limits.",
        "Not verified: the credential is validated; the endpoint answered requests with and without it.",
        "Not verified: chat requests from the phone context, and for the five other listed models.",
        "Off the home network the configured hostname answers 302 (POST included) toward the tailnet name; the phone base URL must be the tailnet hostname with Tailscale connected.",
        "Not verified: behaviour with Tailscale disconnected; the endpoint is unreachable and captures stay queued.",
    ]
}

/// Capability a profile may declare for text interpretation with `model`.
///
/// Only [`CHAT_VERIFIED_MODEL`] is `Supported`, citing the adapter smoke artifact (revision and
/// collection time) and the V08a probe artifact. Other listed models and unlisted models are `Unverified` with a reason, so
/// the dispatcher refuses to use them until evidence exists.
pub fn text_interpretation_capability(model: &str, input_size_limit: usize) -> CapabilityMetadata {
    let capability = if model == CHAT_VERIFIED_MODEL {
        CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            format!(
                "{SMOKE_EVIDENCE_ID} ({SMOKE_EVIDENCE_ARTIFACT}), adapter revision \
                 {SMOKE_ADAPTER_REVISION}, collected {SMOKE_EVIDENCE_COLLECTED_AT}; \
                 {WORKER_EVIDENCE_ID} ({WORKER_EVIDENCE_ARTIFACT}), probe revision \
                 {WORKER_PROBE_REVISION}, collected {WORKER_EVIDENCE_COLLECTED_AT}"
            ),
        )
    } else if LISTED_MODELS.contains(&model) {
        CapabilityMetadata::unverified(
            ProviderCapability::TextInterpretation,
            format!(
                "model is listed by the endpoint in {WORKER_EVIDENCE_ID} but received no chat \
                 request in the probe"
            ),
        )
    } else {
        CapabilityMetadata::unverified(
            ProviderCapability::TextInterpretation,
            format!("model is not listed by the endpoint in {WORKER_EVIDENCE_ID}"),
        )
    };
    capability
        .with_input_size_limit(input_size_limit)
        .with_structured_output(StructuredOutputMode::JsonObject)
}

/// Every capability a self-hosted profile should declare: text interpretation for `model` plus
/// explicit `Unsupported` entries for the capabilities the evidence does not cover.
pub fn declared_capabilities(model: &str, input_size_limit: usize) -> Vec<CapabilityMetadata> {
    let reason = format!("{WORKER_EVIDENCE_ID} exercised only chat completions");
    vec![
        text_interpretation_capability(model, input_size_limit),
        CapabilityMetadata::unsupported(ProviderCapability::Transcription, reason.clone()),
        CapabilityMetadata::unsupported(ProviderCapability::Embeddings, reason.clone()),
        CapabilityMetadata::unsupported(ProviderCapability::SpeechGeneration, reason),
    ]
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    response_format: ResponseFormat,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    role: Option<String>,
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

pub struct SelfHostedAdapter<T: HttpTransport> {
    transport: T,
}

impl<T: HttpTransport> SelfHostedAdapter<T> {
    pub fn new(transport: T) -> Self {
        SelfHostedAdapter { transport }
    }
}

/// `<base>/chat/completions` for a validated https base URL, or `None` when the base carries a
/// query or fragment that would make appending a path ambiguous.
fn chat_completions_url(base_url: &str) -> Option<String> {
    if base_url.contains(['?', '#']) {
        return None;
    }
    Some(format!(
        "{}{CHAT_COMPLETIONS_PATH}",
        base_url.trim_end_matches('/')
    ))
}

impl<T: HttpTransport> ProviderAdapter for SelfHostedAdapter<T> {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        // There is deliberately no fallback endpoint: a profile of another protocol, or one
        // without its own endpoint, is rejected before anything is sent.
        if call.profile.protocol() != ProviderProtocol::SelfHosted {
            return Err(TransportError::Rejected);
        }
        let base_url = call.profile.endpoint().ok_or(TransportError::Rejected)?;
        let url = chat_completions_url(base_url).ok_or(TransportError::Rejected)?;
        let credential_ref = call.profile.credential_ref().as_str();

        let prompt = render_prompt(call.request).map_err(|_| TransportError::Rejected)?;
        let chat_request = ChatRequest {
            model: call.profile.model().to_string(),
            messages: vec![
                Message {
                    role: "system",
                    content: prompt.system,
                },
                Message {
                    role: "user",
                    content: prompt.user,
                },
            ],
            response_format: ResponseFormat {
                kind: "json_object",
            },
            max_tokens: MAX_COMPLETION_TOKENS,
            temperature: 0.0,
            stream: false,
        };
        let body = serde_json::to_vec(&chat_request).map_err(|_| TransportError::Rejected)?;

        let max_response_bytes = call.max_response_bytes as u64;
        let (status, response_bytes) = self.transport.post(
            &url,
            credential_ref,
            &[("Content-Type", "application/json")],
            body,
            call.deadline_ms,
            call.cancel,
            call.clock,
            max_response_bytes,
        )?;

        let within_bound = response_bytes.len() as u64 <= max_response_bytes;
        if (200..300).contains(&status) && !within_bound {
            // Returned untouched so that `dispatch` reports OutputTooLarge.
            return Ok(response_bytes);
        }
        decode_response(status, &response_bytes)
    }
}

fn decode_response(status: u16, response_bytes: &[u8]) -> Result<Vec<u8>, TransportError> {
    if !(200..300).contains(&status) {
        return Err(map_status(status));
    }

    let response: ChatResponse =
        serde_json::from_slice(response_bytes).map_err(|_| TransportError::InvalidOutput)?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(TransportError::InvalidOutput)?;

    // V08a classifies any message that is not an assistant message as incompatible.
    if choice.message.role.as_deref() != Some("assistant") {
        return Err(TransportError::InvalidOutput);
    }
    if choice.message.refusal.is_some() || choice.finish_reason.as_deref() == Some("content_filter")
    {
        return Err(TransportError::Rejected);
    }
    // The probe artifact does not record finish_reason, so an absent value is tolerated; any
    // present value other than a completed stop (length, tool calls, ...) is not a final answer.
    if choice
        .finish_reason
        .as_deref()
        .is_some_and(|reason| reason != "stop")
    {
        return Err(TransportError::InvalidOutput);
    }
    let content = choice
        .message
        .content
        .ok_or(TransportError::InvalidOutput)?;
    Ok(normalize_optional_nulls(content, &output_schema()).into_bytes())
}

/// Treats a `null` in an optional position of `content` as absent, judged against `schema`.
/// Content that is not JSON is returned unchanged for the existing validation to reject; a
/// `null` in a required position, or any non-null value, is left exactly as received.
pub fn normalize_optional_nulls(content: String, schema: &Value) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(&content) else {
        return content;
    };
    drop_optional_nulls(&mut value, schema);
    serde_json::to_string(&value).unwrap_or(content)
}

fn drop_optional_nulls(value: &mut Value, schema: &Value) {
    match value {
        Value::Object(members) => {
            let object_schema = schema
                .get("properties")
                .map(|_| schema)
                .or_else(|| branches_of(schema).find(|branch| branch.get("properties").is_some()));
            let Some(object_schema) = object_schema else {
                return;
            };
            let properties = object_schema["properties"].as_object();
            let required: Vec<&str> = object_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|keys| keys.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            members.retain(|name, member| {
                !member.is_null()
                    || required.contains(&name.as_str())
                    || !properties.is_some_and(|known| known.contains_key(name))
            });
            for (name, member) in members.iter_mut() {
                if let Some(property_schema) = properties.and_then(|known| known.get(name)) {
                    drop_optional_nulls(member, property_schema);
                }
            }
        }
        Value::Array(elements) => {
            if let Some(items) = schema.get("items") {
                elements
                    .iter_mut()
                    .for_each(|element| drop_optional_nulls(element, items));
            }
        }
        _ => {}
    }
}

fn branches_of(schema: &Value) -> impl Iterator<Item = &Value> {
    ["anyOf", "oneOf"]
        .into_iter()
        .filter_map(|keyword| schema.get(keyword).and_then(Value::as_array))
        .flatten()
}

/// Status decides the class. A redirect or server error means the private endpoint is not
/// reachable now (away from the network, or a forwarder answering instead); both are retried
/// later and keep the capture queued. Other client errors are permanent.
fn map_status(status: u16) -> TransportError {
    match status {
        401 | 403 => TransportError::Unauthorized,
        429 => TransportError::RateLimited,
        300..=399 | 408 | 500..=599 => TransportError::Unavailable,
        _ => TransportError::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normalized(content: Value) -> Value {
        let text = normalize_optional_nulls(content.to_string(), &output_schema());
        serde_json::from_str(&text).expect("still JSON")
    }

    #[test]
    fn top_level_nulls_become_absent() {
        let reply = json!({
            "operation": {"kind": "annotate"},
            "item_type": "idea",
            "reminder_proposal": null,
            "session_topic_proposal": null,
            "abstention": null
        });
        assert_eq!(
            normalized(reply),
            json!({"operation": {"kind": "annotate"}, "item_type": "idea"})
        );
    }

    #[test]
    fn nested_nulls_become_absent() {
        let reply = json!({
            "operation": {"kind": "annotate"},
            "item_type": "action",
            "source_spans": [{"start": 0, "end": 3}],
            "reminder_proposal": {
                "quality": "ambiguous",
                "instant": null,
                "timezone_id": null,
                "source_span": {"start": 1, "end": 2}
            }
        });
        assert_eq!(
            normalized(reply)["reminder_proposal"],
            json!({"quality": "ambiguous", "source_span": {"start": 1, "end": 2}})
        );
    }

    #[test]
    fn values_validation_rejects_are_left_untouched() {
        let reply = json!({
            "operation": null,
            "reminder_proposal": {"quality": null, "source_span": null, "instant": 5},
            "abstention": {"Other": null},
            "unknown": null
        });
        let result = normalized(reply);
        assert!(result["operation"].is_null());
        assert!(result["reminder_proposal"]["quality"].is_null());
        assert!(result["reminder_proposal"]["source_span"].is_null());
        assert_eq!(result["reminder_proposal"]["instant"], json!(5));
        assert!(result["abstention"]["Other"].is_null());
        assert!(
            result.get("unknown").is_some(),
            "unknown keys are not mine to drop"
        );
        assert_eq!(
            normalize_optional_nulls("not json".to_string(), &output_schema()),
            "not json"
        );
    }
}
