//! The verified self-hosted adapter, registered for the benchmark.
//!
//! The adapter, its capability declaration and its compatibility boundary are core's (V09). This
//! module only supplies what a profile cannot carry itself: a validated endpoint and the adapter
//! over the host transport. What the adapter can honor for a diagnostic call is whatever it
//! declares; nothing is assumed from the protocol label.

use ohand_core::providers::contracts::{
    CancelToken, CapabilityMetadata, Clock, DiagnosticSupport, ProviderAdapter, TransportError,
};
use ohand_core::providers::openai::HttpTransport;
use ohand_core::providers::self_hosted::{declared_capabilities, SelfHostedAdapter};
use url::Url;

use crate::transport::HostTransport;

/// A self-hosted base URL: https, a host, no userinfo, query or fragment. The adapter posts to
/// `<base>/chat/completions`, so the whole base is pinned, not only its origin.
#[derive(Clone, PartialEq, Eq)]
pub struct SelfHostedEndpoint {
    base_url: String,
    origin: String,
}

/// The endpoint is private configuration, so it is never shown, even in debug output.
impl std::fmt::Debug for SelfHostedEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelfHostedEndpoint")
            .finish_non_exhaustive()
    }
}

impl SelfHostedEndpoint {
    /// Errors describe the shape only, never the value.
    pub fn parse(text: &str) -> Result<SelfHostedEndpoint, String> {
        let url = Url::parse(text.trim()).map_err(|_| "the endpoint is not a URL".to_string())?;
        if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
            return Err("the endpoint must be an https URL".to_string());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("the endpoint must not carry credentials".to_string());
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err("the endpoint must not have a query or fragment".to_string());
        }
        Ok(SelfHostedEndpoint {
            base_url: url.as_str().trim_end_matches('/').to_string(),
            origin: url.origin().ascii_serialization(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }
}

/// Core's declaration for `model`: only the model with run evidence is `Supported`.
pub fn capabilities(model: &str, input_size_limit: usize) -> Vec<CapabilityMetadata> {
    declared_capabilities(model, input_size_limit)
}

/// Answers the capability question without a transport, so planning and fake mode report what
/// live mode would find.
struct NoTransport;

impl HttpTransport for NoTransport {
    fn post(
        &self,
        _endpoint: &str,
        _credential_ref: &str,
        _headers: &[(&str, &str)],
        _body: Vec<u8>,
        _deadline_ms: u64,
        _cancel: &CancelToken,
        _clock: &dyn Clock,
        _max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        Err(TransportError::Unavailable)
    }
}

/// Diagnostic calls the production adapter declares it can honor.
pub fn diagnostic_support() -> DiagnosticSupport {
    SelfHostedAdapter::new(NoTransport).diagnostic_support()
}

/// The production adapter over the host transport. It contacts only the profile's own endpoint.
pub fn adapter(transport: &HostTransport) -> Box<dyn ProviderAdapter> {
    Box::new(SelfHostedAdapter::new(transport.clone()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ohand_core::interpretation::instructions::M1_INSTRUCTION_VERSION;
    use ohand_core::providers::contracts::{
        AdapterCall, CapabilitySupport, InterpretationRequest, ProviderCapability, ProviderProfile,
        ProviderProfileBuilder, ProviderProtocol, SystemClock, TextBasis,
    };
    use ohand_core::providers::self_hosted::{CHAT_VERIFIED_MODEL, LISTED_MODELS, SMOKE_EVIDENCE};
    use ohand_core::time::TimeContext;
    use uuid::Uuid;

    use super::*;
    use crate::transport::{CredentialError, CredentialResolver, DestinationPolicy, SecretValue};

    use crate::adapters::fixture::{Fixture, Reply, TestAuthority};

    const SECRET: &str = "synthetic-secret-spark-0001";
    const REFERENCE: &str = "bench-self-hosted";
    const COMPLETION: &[u8] = br#"{"choices":[{"message":{"role":"assistant","content":"{\"operation\":{\"kind\":\"annotate\"}}"},"finish_reason":"stop"}]}"#;

    struct MappedResolver(HashMap<String, String>);

    impl CredentialResolver for MappedResolver {
        fn resolve(&self, reference: &str) -> Result<SecretValue, CredentialError> {
            let value = self.0.get(reference).ok_or(CredentialError::NotFound)?;
            SecretValue::from_bytes(value.clone().into_bytes())
        }
    }

    fn resolver(with_secret: bool) -> Arc<MappedResolver> {
        let mut secrets = HashMap::new();
        if with_secret {
            secrets.insert(REFERENCE.to_string(), SECRET.to_string());
        }
        Arc::new(MappedResolver(secrets))
    }

    fn transport(
        authority: &TestAuthority,
        policy: DestinationPolicy,
        with_secret: bool,
    ) -> HostTransport {
        HostTransport::builder(policy, resolver(with_secret))
            .trust_root_der(authority.root_der())
            .build()
            .unwrap()
    }

    fn profile(model: &str, base_url: &str) -> ProviderProfile {
        let endpoint = SelfHostedEndpoint::parse(base_url).unwrap();
        let mut builder = ProviderProfileBuilder::new("spark", ProviderProtocol::SelfHosted, model)
            .endpoint(endpoint.base_url())
            .credential_ref(REFERENCE)
            .authorized_destination(endpoint.origin());
        for capability in capabilities(model, 8_000) {
            builder = builder.capability(capability);
        }
        builder.build().unwrap()
    }

    fn request(profile: &ProviderProfile) -> InterpretationRequest {
        let time_context: TimeContext = serde_json::from_value(serde_json::json!({
            "timezone": "UTC",
            "locale": "en-US",
            "reference_time": "2026-01-05T09:00:00Z",
            "utc_offset_at_capture": 0,
            "calendar": "gregorian",
        }))
        .unwrap();
        InterpretationRequest::new(
            Uuid::new_v4().to_string(),
            0,
            TextBasis::Original { item_revision: 0 },
            "Synthetic note: water the plants on Friday.",
            Uuid::new_v4().to_string(),
            M1_INSTRUCTION_VERSION,
            profile,
            "bench-route",
            time_context,
        )
        .unwrap()
    }

    fn invoke(
        adapter: &dyn ProviderAdapter,
        profile: &ProviderProfile,
    ) -> Result<Vec<u8>, TransportError> {
        let request = request(profile);
        let cancel = CancelToken::new();
        let clock = SystemClock::new();
        adapter.invoke(&AdapterCall {
            request: &request,
            profile,
            deadline_ms: clock.now_ms() + 20_000,
            max_response_bytes: 1 << 20,
            cancel: &cancel,
            clock: &clock,
        })
    }

    #[test]
    fn endpoint_accepts_an_https_base_and_pins_all_of_it() {
        let endpoint = SelfHostedEndpoint::parse("https://spark.tailnet.example:8443/v1/").unwrap();
        assert_eq!(endpoint.base_url(), "https://spark.tailnet.example:8443/v1");
        assert_eq!(endpoint.origin(), "https://spark.tailnet.example:8443");
        let debug = format!("{endpoint:?}");
        assert!(
            !debug.contains("tailnet") && !debug.contains("8443"),
            "{debug}"
        );
    }

    #[test]
    fn endpoint_refuses_unsafe_shapes_without_repeating_them() {
        for text in [
            "",
            "spark",
            "http://spark.tailnet.example/v1",
            "ftp://spark.tailnet.example/v1",
            "https://user:pass-canary@spark.tailnet.example/v1",
            "https://spark.tailnet.example/v1?token=canary",
            "https://spark.tailnet.example/v1#canary",
        ] {
            let error = SelfHostedEndpoint::parse(text).unwrap_err();
            assert!(
                !error.contains("canary") && !error.contains("tailnet"),
                "{error}"
            );
        }
    }

    #[test]
    fn only_the_model_with_run_evidence_is_declared_supported() {
        let support_of = |model: &str| {
            capabilities(model, 8_000)
                .into_iter()
                .find(|capability| capability.capability == ProviderCapability::TextInterpretation)
                .expect("text interpretation is always declared")
        };
        for smoke in SMOKE_EVIDENCE {
            let verified = support_of(smoke.model);
            assert_eq!(verified.support, CapabilitySupport::Supported);
            assert!(verified
                .evidence
                .as_deref()
                .is_some_and(|evidence| evidence.contains(smoke.id)));
        }
        for model in LISTED_MODELS
            .iter()
            .filter(|model| SMOKE_EVIDENCE.iter().all(|smoke| smoke.model != **model))
        {
            let declared = support_of(model);
            assert_eq!(declared.support, CapabilitySupport::Unverified, "{model}");
            assert!(declared.reason.is_some());
        }
        assert_eq!(
            support_of("not-listed-model").support,
            CapabilitySupport::Unverified
        );
    }

    #[test]
    fn registered_adapter_posts_the_configured_model_only_to_the_configured_endpoint() {
        let authority = TestAuthority::new();
        let spark = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let cloud = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let base_url = spark.url("/v1");
        let policy = DestinationPolicy::new()
            .approve_credential(&base_url, REFERENCE)
            .unwrap();
        let adapter = adapter(&transport(&authority, policy, true));
        let profile = profile(CHAT_VERIFIED_MODEL, &base_url);

        let body = invoke(adapter.as_ref(), &profile).expect("the fixture answers");
        assert!(String::from_utf8(body).unwrap().contains("annotate"));

        let requests = spark.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].request_line.contains("/v1/chat/completions"),
            "{}",
            requests[0].request_line
        );
        assert_eq!(
            requests[0].header("authorization"),
            Some(format!("Bearer {SECRET}").as_str())
        );
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(sent["model"], CHAT_VERIFIED_MODEL);
        assert_eq!(cloud.connections(), 0);
    }

    #[test]
    fn a_server_failure_is_unavailable_and_nothing_else_is_contacted() {
        let authority = TestAuthority::new();
        let spark = Fixture::start(authority.issue("localhost"), |_| Reply::status(503, b"{}"));
        let cloud = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let base_url = spark.url("/v1");
        let policy = DestinationPolicy::new()
            .approve_credential(&base_url, REFERENCE)
            .unwrap()
            .approve_credential(&cloud.url("/v1"), REFERENCE)
            .unwrap();
        let adapter = adapter(&transport(&authority, policy, true));
        let profile = profile(CHAT_VERIFIED_MODEL, &base_url);

        let error = invoke(adapter.as_ref(), &profile).unwrap_err();
        assert_eq!(error, TransportError::Unavailable);
        assert_eq!(spark.requests().len(), 1, "no retry inside one call");
        assert_eq!(cloud.connections(), 0, "no fallback to another destination");
    }

    #[test]
    fn an_unreachable_server_fails_and_nothing_else_is_contacted() {
        let authority = TestAuthority::new();
        let cloud = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let base_url = {
            let gone = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
            gone.url("/v1")
        };
        let policy = DestinationPolicy::new()
            .approve_credential(&base_url, REFERENCE)
            .unwrap()
            .approve_credential(&cloud.url("/v1"), REFERENCE)
            .unwrap();
        let adapter = adapter(&transport(&authority, policy, true));
        let profile = profile(CHAT_VERIFIED_MODEL, &base_url);

        assert!(invoke(adapter.as_ref(), &profile).is_err());
        assert_eq!(cloud.connections(), 0);
    }

    #[test]
    fn a_missing_credential_or_ungranted_destination_sends_nothing() {
        let authority = TestAuthority::new();
        let spark = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let base_url = spark.url("/v1");
        let profile = profile(CHAT_VERIFIED_MODEL, &base_url);

        let granted = DestinationPolicy::new()
            .approve_credential(&base_url, REFERENCE)
            .unwrap();
        let without_secret = adapter(&transport(&authority, granted, false));
        assert!(invoke(without_secret.as_ref(), &profile).is_err());

        let ungranted = adapter(&transport(&authority, DestinationPolicy::new(), true));
        assert!(invoke(ungranted.as_ref(), &profile).is_err());

        assert_eq!(spark.connections(), 0);
    }

    #[test]
    fn a_profile_of_another_protocol_is_refused_before_anything_is_sent() {
        let authority = TestAuthority::new();
        let spark = Fixture::start(authority.issue("localhost"), |_| Reply::ok(COMPLETION));
        let policy = DestinationPolicy::new()
            .approve_credential(&spark.url("/v1"), REFERENCE)
            .unwrap();
        let adapter = adapter(&transport(&authority, policy, true));
        let hosted = ProviderProfileBuilder::new("hosted", ProviderProtocol::OpenAi, "model")
            .credential_ref(REFERENCE)
            .authorized_destination("https://api.openai.com")
            .capability(
                ohand_core::providers::contracts::CapabilityMetadata::supported(
                    ProviderCapability::TextInterpretation,
                    "synthetic",
                )
                .with_input_size_limit(8_000),
            )
            .build()
            .unwrap();
        assert_eq!(
            invoke(adapter.as_ref(), &hosted).unwrap_err(),
            TransportError::Rejected
        );
        assert_eq!(spark.connections(), 0);
    }
}
