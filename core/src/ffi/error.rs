// Normalized error classes for FFI boundary.
// Matches the architecture contract (Normalized errors and lifetime rules).

/// Normalized error class crossing the ABI.
/// Every effect returns success or one of these classes.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Success (no error).
    Ok = 0,
    /// Transient: retry with backoff.
    Transient = 1,
    /// Permanent: surface to the user; no retry.
    Permanent = 2,
    /// Unauthorized: permission or credential problem.
    Unauthorized = 3,
    /// Cancelled: operation was cancelled before completion.
    Cancelled = 4,
    /// Unsupported: capability not available.
    Unsupported = 5,
}

/// Convert a Result to an error class.
/// Errors are redacted; diagnostics are never exposed.
pub fn classify_error(error: &anyhow::Error) -> ErrorClass {
    let chain: String = error
        .chain()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(": ");

    if chain.contains("unauthorized")
        || chain.contains("permission")
        || chain.contains("credential")
    {
        ErrorClass::Unauthorized
    } else if chain.contains("cancelled") {
        ErrorClass::Cancelled
    } else if chain.contains("unsupported") {
        ErrorClass::Unsupported
    } else if chain.contains("temporary")
        || chain.contains("timeout")
        || chain.contains("network")
        || chain.contains("transient")
    {
        ErrorClass::Transient
    } else {
        ErrorClass::Permanent
    }
}

/// Redacted error message safe to expose (no secrets or content).
pub fn redacted_message(error: &anyhow::Error) -> String {
    let class = classify_error(error);
    match class {
        ErrorClass::Ok => "success".to_string(),
        ErrorClass::Transient => "transient failure, will retry".to_string(),
        ErrorClass::Permanent => "operation failed".to_string(),
        ErrorClass::Unauthorized => "not authorized".to_string(),
        ErrorClass::Cancelled => "cancelled".to_string(),
        ErrorClass::Unsupported => "not supported".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        let unauthorized = anyhow::anyhow!("unauthorized access");
        assert_eq!(classify_error(&unauthorized), ErrorClass::Unauthorized);

        let cancelled = anyhow::anyhow!("operation cancelled");
        assert_eq!(classify_error(&cancelled), ErrorClass::Cancelled);

        let unsupported = anyhow::anyhow!("unsupported feature");
        assert_eq!(classify_error(&unsupported), ErrorClass::Unsupported);

        let transient = anyhow::anyhow!("network timeout");
        assert_eq!(classify_error(&transient), ErrorClass::Transient);

        let permanent = anyhow::anyhow!("database error");
        assert_eq!(classify_error(&permanent), ErrorClass::Permanent);
    }

    #[test]
    fn test_redacted_message_no_content() {
        let with_content = anyhow::anyhow!("Failed to process user's private text");
        let msg = redacted_message(&with_content);
        assert!(!msg.contains("private"));
        assert!(!msg.contains("user"));
    }
}
