use std::fmt;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffRoute {
    Capture,
}

impl HandoffRoute {
    pub fn as_str(&self) -> &'static str {
        match self {
            HandoffRoute::Capture => "capture",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "capture" => Some(HandoffRoute::Capture),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    pub capture_id: String,
    pub route: HandoffRoute,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    InvalidUrl,
    MissingCaptureId,
    InvalidRoute,
    MissingRoute,
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HandoffError::InvalidUrl => write!(f, "Invalid handoff URL"),
            HandoffError::MissingCaptureId => write!(f, "Missing captureId parameter"),
            HandoffError::InvalidRoute => write!(f, "Invalid route parameter"),
            HandoffError::MissingRoute => write!(f, "Missing route in URL path"),
        }
    }
}

pub struct HandoffValidator;

impl HandoffValidator {
    const SCHEME: &'static str = "ohand-tauri";

    pub fn validate(url: &str) -> Result<HandoffRequest, HandoffError> {
        let parsed_url = Url::parse(url).map_err(|_| HandoffError::InvalidUrl)?;

        if parsed_url.scheme() != Self::SCHEME {
            return Err(HandoffError::InvalidUrl);
        }

        let route_component = parsed_url.host_str()
            .ok_or(HandoffError::MissingRoute)?;

        if route_component.is_empty() {
            return Err(HandoffError::MissingRoute);
        }

        let route = HandoffRoute::from_str(route_component)
            .ok_or(HandoffError::InvalidRoute)?;

        let query_pairs: Vec<(String, String)> = parsed_url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let capture_id = query_pairs
            .iter()
            .find(|(k, _)| k == "captureId")
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
            .ok_or(HandoffError::MissingCaptureId)?;

        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        Ok(HandoffRequest {
            capture_id,
            route,
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_capture_handoff() {
        let url = "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert_eq!(request.capture_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(request.route, HandoffRoute::Capture);
    }

    #[test]
    fn test_missing_capture_id() {
        let url = "ohand-tauri://capture";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::MissingCaptureId));
    }

    #[test]
    fn test_empty_capture_id() {
        let url = "ohand-tauri://capture?captureId=";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::MissingCaptureId));
    }

    #[test]
    fn test_invalid_route() {
        let url = "ohand-tauri://invalid?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::InvalidRoute));
    }

    #[test]
    fn test_missing_route() {
        let url = "ohand-tauri://?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::MissingRoute));
    }

    #[test]
    fn test_wrong_scheme() {
        let url = "ohand://capture?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_malicious_route_injection() {
        let url = "ohand-tauri://../../settings?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::InvalidRoute));
    }

    #[test]
    fn test_multiple_capture_id_parameters() {
        let url = "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000&captureId=evil";
        let result = HandoffValidator::validate(url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert_eq!(request.capture_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_timestamp_is_set() {
        let url = "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000";
        let result = HandoffValidator::validate(url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert!(!request.timestamp.is_empty());
    }
}
