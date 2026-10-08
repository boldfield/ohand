use std::fmt;
use std::str::FromStr;

const URL_PREFIX: &str = "ohand-tauri://";
const ROUTE: &str = "capture";
const QUERY_KEY_PREFIX: &str = "captureId=";
const UUID_LENGTH: usize = 36;
const UUID_HYPHEN_POSITIONS: [usize; 4] = [8, 13, 18, 23];

/// Why a handoff URL or identifier was refused. The codes are shared with the Swift implementation and the fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    BadScheme,
    UnknownRoute,
    MissingCaptureId,
    UnexpectedComponent,
    InvalidCaptureId,
}

impl HandoffError {
    pub fn code(self) -> &'static str {
        match self {
            HandoffError::BadScheme => "bad_scheme",
            HandoffError::UnknownRoute => "unknown_route",
            HandoffError::MissingCaptureId => "missing_capture_id",
            HandoffError::UnexpectedComponent => "unexpected_component",
            HandoffError::InvalidCaptureId => "invalid_capture_id",
        }
    }
}

impl fmt::Display for HandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for HandoffError {}

/// A capture identifier in the exact form the native entry mints it: upper-case, hyphenated, 8-4-4-4-12 hex.
/// Because only `0-9A-F` and hyphens are possible, a `CaptureId` is always safe to use as a file name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CaptureId(String);

impl CaptureId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CaptureId {
    type Err = HandoffError;

    fn from_str(candidate: &str) -> Result<Self, Self::Err> {
        let is_canonical = candidate.len() == UUID_LENGTH
            && candidate.bytes().enumerate().all(|(index, byte)| {
                if UUID_HYPHEN_POSITIONS.contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
                }
            });
        if is_canonical {
            Ok(CaptureId(candidate.to_owned()))
        } else {
            Err(HandoffError::InvalidCaptureId)
        }
    }
}

impl fmt::Display for CaptureId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn build_handoff_url(capture_id: &CaptureId) -> String {
    format!("{URL_PREFIX}{ROUTE}?{QUERY_KEY_PREFIX}{capture_id}")
}

/// Accepts exactly `ohand-tauri://capture?captureId=<canonical id>` and nothing else: no extra path, user info, port,
/// fragment, second parameter, repeated parameter, other casing or encoding.
pub fn parse_handoff_url(url: &str) -> Result<CaptureId, HandoffError> {
    let after_scheme = url
        .strip_prefix(URL_PREFIX)
        .ok_or(HandoffError::BadScheme)?;
    let (route, query) = match after_scheme.split_once('?') {
        Some((route, query)) => (route, Some(query)),
        None => (after_scheme, None),
    };
    if route != ROUTE {
        return Err(HandoffError::UnknownRoute);
    }
    let query = match query {
        None | Some("") => return Err(HandoffError::MissingCaptureId),
        Some(query) => query,
    };
    let candidate = query
        .strip_prefix(QUERY_KEY_PREFIX)
        .ok_or(HandoffError::UnexpectedComponent)?;
    candidate.parse()
}
