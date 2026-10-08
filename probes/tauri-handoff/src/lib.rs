use std::fmt;
use std::str::FromStr;
use url::Url;
use uuid::Uuid;

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
}

impl FromStr for HandoffRoute {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "capture" => Ok(HandoffRoute::Capture),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    pub capture_id: Uuid,
    pub route: HandoffRoute,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    InvalidUrl,
    MissingCaptureId,
    InvalidCaptureId,
    DuplicateCaptureId,
    InvalidRoute,
    MissingRoute,
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HandoffError::InvalidUrl => write!(f, "Invalid handoff URL"),
            HandoffError::MissingCaptureId => write!(f, "Missing captureId parameter"),
            HandoffError::InvalidCaptureId => write!(f, "Invalid captureId: must be a valid UUID"),
            HandoffError::DuplicateCaptureId => write!(f, "Duplicate captureId parameters"),
            HandoffError::InvalidRoute => write!(f, "Invalid route parameter"),
            HandoffError::MissingRoute => write!(f, "Missing route in URL host"),
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

        if parsed_url.username() != "" || parsed_url.password().is_some() {
            return Err(HandoffError::InvalidUrl);
        }

        if parsed_url.port().is_some() {
            return Err(HandoffError::InvalidUrl);
        }

        if parsed_url.fragment().is_some() {
            return Err(HandoffError::InvalidUrl);
        }

        let path = parsed_url.path();
        if !path.is_empty() && path != "/" {
            return Err(HandoffError::InvalidUrl);
        }

        let route_component = parsed_url.host_str().ok_or(HandoffError::MissingRoute)?;

        if route_component.is_empty() {
            return Err(HandoffError::MissingRoute);
        }

        let route = route_component
            .parse::<HandoffRoute>()
            .map_err(|_| HandoffError::InvalidRoute)?;

        let query_pairs: Vec<(String, String)> = parsed_url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let capture_id_values: Vec<&String> = query_pairs
            .iter()
            .filter_map(|(k, v)| if k == "captureId" { Some(v) } else { None })
            .collect();

        if capture_id_values.is_empty() {
            return Err(HandoffError::MissingCaptureId);
        }

        if capture_id_values.len() > 1 {
            return Err(HandoffError::DuplicateCaptureId);
        }

        for (k, _) in query_pairs.iter() {
            if k != "captureId" {
                return Err(HandoffError::InvalidUrl);
            }
        }

        let capture_id_str = capture_id_values[0];
        if capture_id_str.is_empty() {
            return Err(HandoffError::MissingCaptureId);
        }

        let capture_id =
            Uuid::parse_str(capture_id_str).map_err(|_| HandoffError::InvalidCaptureId)?;

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

    const VALID_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn test_valid_capture_handoff() {
        let url = format!("ohand-tauri://capture?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert_eq!(request.capture_id, Uuid::parse_str(VALID_UUID).unwrap());
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
    fn test_invalid_capture_id() {
        let url = "ohand-tauri://capture?captureId=not-a-uuid";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::InvalidCaptureId));
    }

    #[test]
    fn test_malicious_capture_id() {
        let url = "ohand-tauri://capture?captureId=<script>";
        let result = HandoffValidator::validate(url);
        assert_eq!(result, Err(HandoffError::InvalidCaptureId));
    }

    #[test]
    fn test_invalid_route() {
        let url = format!("ohand-tauri://invalid?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidRoute));
    }

    #[test]
    fn test_missing_route() {
        let url = format!("ohand-tauri://?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::MissingRoute));
    }

    #[test]
    fn test_wrong_scheme() {
        let url = format!("ohand://capture?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_malicious_route_injection() {
        let url = format!("ohand-tauri://../../settings?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_rejects_path_with_traversal() {
        let url = format!("ohand-tauri://capture/../../admin?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_multiple_capture_id_parameters() {
        let url = format!(
            "ohand-tauri://capture?captureId={}&captureId=evil",
            VALID_UUID
        );
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::DuplicateCaptureId));
    }

    #[test]
    fn test_timestamp_is_set() {
        let url = format!("ohand-tauri://capture?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert!(!request.timestamp.is_empty());
    }

    #[test]
    fn test_rejects_url_with_path() {
        let url = format!("ohand-tauri://capture/settings?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_rejects_url_with_user_info() {
        let url = format!("ohand-tauri://user@capture?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_rejects_url_with_port() {
        let url = format!("ohand-tauri://capture:8080?captureId={}", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_rejects_url_with_fragment() {
        let url = format!("ohand-tauri://capture?captureId={}#section", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }

    #[test]
    fn test_rejects_url_with_extra_query_parameters() {
        let url = format!("ohand-tauri://capture?captureId={}&extra=value", VALID_UUID);
        let result = HandoffValidator::validate(&url);
        assert_eq!(result, Err(HandoffError::InvalidUrl));
    }
}
