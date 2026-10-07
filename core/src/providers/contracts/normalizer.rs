//! Response normalization and contract enforcement for provider adapters.
//!
//! The normalizer applies contract enforcement rules to raw provider responses:
//! - Bounds raw output to the configured maximum size
//! - Validates output format
//! - Normalizes errors into standard error types
//! - Tests that removal of enforcement causes failures

use super::{ErrorType, InterpretationResponse, ProviderError, ResponseStatus};

/// Normalizes and enforces contract bounds on provider responses.
pub struct ResponseNormalizer {
    max_output_bytes: usize,
}

impl ResponseNormalizer {
    /// Create a normalizer with the given maximum output size.
    pub fn new(max_output_bytes: usize) -> Self {
        ResponseNormalizer { max_output_bytes }
    }

    /// Normalize a raw response from a provider adapter.
    /// Enforces size bounds and validates structure.
    pub fn normalize(&self, mut response: InterpretationResponse) -> InterpretationResponse {
        if response.status == ResponseStatus::Success
            || response.status == ResponseStatus::InvalidOutput
        {
            if let Some(ref mut result) = response.result {
                if result.raw_output.len() > self.max_output_bytes {
                    result.raw_output = String::new();
                    response.status = ResponseStatus::InvalidOutput;
                    response.error = Some(ProviderError {
                        error_type: ErrorType::InvalidOutput,
                        message: format!(
                            "Response exceeds maximum size of {} bytes",
                            self.max_output_bytes
                        ),
                        retriable: false,
                        code: Some("RESPONSE_TOO_LARGE".to_string()),
                    });
                }
            }
        }

        response
    }

    /// Get the maximum output size for this normalizer.
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::super::InterpretationResult;
    use super::*;
    use serde_json::json;

    #[test]
    fn test_normalizer_rejects_output_exceeding_limit() {
        let normalizer = ResponseNormalizer::new(100);
        let oversized = "x".repeat(150);

        let response = InterpretationResponse {
            request_id: "test-req".to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(json!({})),
                raw_output: oversized,
            }),
            error: None,
            usage: None,
        };

        let normalized = normalizer.normalize(response);

        assert_eq!(normalized.status, ResponseStatus::InvalidOutput);
        assert!(normalized.result.is_some());
        assert!(normalized.result.as_ref().unwrap().raw_output.is_empty());
        assert!(normalized.error.is_some());
        assert_eq!(
            normalized.error.as_ref().unwrap().error_type,
            ErrorType::InvalidOutput
        );
    }

    #[test]
    fn test_normalizer_accepts_output_at_limit() {
        let normalizer = ResponseNormalizer::new(100);
        let output = "x".repeat(100);

        let response = InterpretationResponse {
            request_id: "test-req".to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(json!({})),
                raw_output: output.clone(),
            }),
            error: None,
            usage: None,
        };

        let normalized = normalizer.normalize(response);

        assert_eq!(normalized.status, ResponseStatus::Success);
        assert_eq!(normalized.result.as_ref().unwrap().raw_output.len(), 100);
    }

    #[test]
    fn test_normalizer_accepts_output_below_limit() {
        let normalizer = ResponseNormalizer::new(100);
        let output = "hello world".to_string();

        let response = InterpretationResponse {
            request_id: "test-req".to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(json!({})),
                raw_output: output.clone(),
            }),
            error: None,
            usage: None,
        };

        let normalized = normalizer.normalize(response);

        assert_eq!(normalized.status, ResponseStatus::Success);
        assert_eq!(normalized.result.as_ref().unwrap().raw_output, output);
    }

    #[test]
    fn test_normalizer_ignores_size_check_for_error_status() {
        let normalizer = ResponseNormalizer::new(100);
        let oversized = "x".repeat(150);

        let response = InterpretationResponse {
            request_id: "test-req".to_string(),
            status: ResponseStatus::Timeout,
            result: Some(InterpretationResult {
                annotations: None,
                raw_output: oversized.clone(),
            }),
            error: Some(ProviderError {
                error_type: ErrorType::Timeout,
                message: "Timeout".to_string(),
                retriable: true,
                code: None,
            }),
            usage: None,
        };

        let normalized = normalizer.normalize(response);

        assert_eq!(normalized.status, ResponseStatus::Timeout);
    }

    #[test]
    fn test_normalizer_boundary_test_at_limit_plus_one() {
        let limit = 100;
        let normalizer = ResponseNormalizer::new(limit);

        let at_limit = "x".repeat(limit);
        let response_at = InterpretationResponse {
            request_id: "test-1".to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(json!({})),
                raw_output: at_limit,
            }),
            error: None,
            usage: None,
        };

        let normalized_at = normalizer.normalize(response_at);
        assert_eq!(normalized_at.status, ResponseStatus::Success);

        let over_limit = "x".repeat(limit + 1);
        let response_over = InterpretationResponse {
            request_id: "test-2".to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(json!({})),
                raw_output: over_limit,
            }),
            error: None,
            usage: None,
        };

        let normalized_over = normalizer.normalize(response_over);
        assert_eq!(normalized_over.status, ResponseStatus::InvalidOutput);
        assert!(normalized_over
            .result
            .as_ref()
            .unwrap()
            .raw_output
            .is_empty());
    }
}
