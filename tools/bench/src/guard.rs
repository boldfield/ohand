//! Detection of credentials, private endpoints, and database inputs in benchmark records.
//!
//! The detector is structural rather than substring-based: it tokenizes text, parses URL
//! authorities and IPv4 literals, normalizes key names, and walks JSON trees. Callers get a
//! yes/no answer and never the offending value, so rejection messages cannot leak it.

use serde_json::Value;
use std::net::{Ipv4Addr, Ipv6Addr};

/// The only hosts a synthetic record may reference by URL.
const SYNTHETIC_HOSTS: &[&str] = &[
    "example.com",
    "example.org",
    "example.net",
    "www.example.com",
    "www.example.org",
    "www.example.net",
];

const DATABASE_EXTENSIONS: &[&str] = &[".sqlite", ".sqlite3", ".db", ".db3", ".sqlcipher", ".mdb"];

const PRIVATE_PATH_PREFIXES: &[&str] = &[
    "/var/",
    "/users/",
    "/home/",
    "/private/",
    "/data/",
    "/library/",
    "/root/",
    "/etc/",
    "/mnt/",
    "~/",
];

/// Normalized (lowercase, separators removed) key-name suffixes that always hold a credential.
const SENSITIVE_KEY_SUFFIXES: &[&str] = &[
    "apikey",
    "authorization",
    "password",
    "passwd",
    "secret",
    "token",
    "privatekey",
];

/// Normalized key-name suffixes whose string value must be an approved synthetic endpoint.
const ENDPOINT_KEY_SUFFIXES: &[&str] = &[
    "endpoint", "url", "uri", "host", "hostname", "baseurl", "server", "origin",
];

/// Normalized key names that are sensitive only when they match exactly.
const SENSITIVE_EXACT_KEYS: &[&str] = &["token", "auth", "credentials", "cookie", "setcookie"];

/// Normalized key-name suffixes whose `key: value` header form (no quotes) is a credential for
/// any non-empty value.
const HEADER_KEY_SUFFIXES: &[&str] = &["apikey", "authorization", "cookie"];

/// Prose values that may follow a bare `key:` (for example "password: required") without being a
/// credential or an endpoint. Every other value is treated as one.
const PLACEHOLDER_VALUES: &[&str] = &[
    "required",
    "optional",
    "none",
    "null",
    "nil",
    "unknown",
    "redacted",
    "n/a",
    "na",
    "empty",
    "missing",
    "expired",
    "invalid",
    "revoked",
    "unset",
    "omitted",
    "masked",
    "hidden",
    "placeholder",
    "***",
];

/// Hostname suffixes that only resolve on private networks.
const PRIVATE_HOST_SUFFIXES: &[&str] = &[
    ".internal",
    ".local",
    ".lan",
    ".corp",
    ".home.arpa",
    ".localdomain",
    ".localhost",
    ".intranet",
];

const MAX_JSON_DEPTH: usize = 64;

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_sensitive_key_name(normalized: &str, exact_names_too: bool) -> bool {
    SENSITIVE_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
        || (exact_names_too && SENSITIVE_EXACT_KEYS.contains(&normalized))
}

fn is_endpoint_key_name(normalized: &str) -> bool {
    ENDPOINT_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

/// True if the value names only an approved synthetic host, optionally as an http(s) URL.
fn is_synthetic_endpoint(value: &str) -> bool {
    let trimmed = value.trim();
    let lowered = trimmed.to_ascii_lowercase();
    let without_scheme = lowered
        .strip_prefix("https://")
        .or_else(|| lowered.strip_prefix("http://"))
        .unwrap_or(&lowered);
    let authority_end = without_scheme
        .find(['/', '?', '#', '\\'])
        .unwrap_or(without_scheme.len());
    let authority = &without_scheme[..authority_end];
    if authority.contains('@') {
        return false;
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    SYNTHETIC_HOSTS.contains(&host)
}

fn is_header_key_name(normalized: &str) -> bool {
    HEADER_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

fn is_placeholder_value(value: &str) -> bool {
    PLACEHOLDER_VALUES.contains(&value.to_ascii_lowercase().as_str())
}

/// True if a value following a bare `endpoint:`-style key names a host rather than prose such as
/// "server: unavailable".
fn looks_like_host(value: &str) -> bool {
    value.eq_ignore_ascii_case("localhost")
        || value.contains(['.', ':', '/', '[', '@'])
        || value.parse::<Ipv4Addr>().is_ok()
}

/// True if a string contains a credential, a private/unapproved endpoint, or a database input.
pub fn is_secret_or_endpoint(value: &str) -> bool {
    contains_unapproved_url(value)
        || contains_private_network_address(value)
        || contains_private_hostname(value)
        || contains_database_or_private_path(value)
        || contains_secret_material(value)
}

/// True if any string or key anywhere in the JSON tree trips [`is_secret_or_endpoint`], or any
/// credential-named key carries a value.
pub fn json_is_secret_or_endpoint(value: &Value) -> bool {
    json_violates(value, 0, false)
}

fn json_violates(value: &Value, depth: usize, endpoint_context: bool) -> bool {
    if depth > MAX_JSON_DEPTH {
        return true;
    }
    match value {
        Value::String(text) => {
            is_secret_or_endpoint(text)
                || (endpoint_context && !text.trim().is_empty() && !is_synthetic_endpoint(text))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| json_violates(item, depth + 1, endpoint_context)),
        Value::Object(map) => map.iter().any(|(key, inner)| {
            if is_secret_or_endpoint(key) {
                return true;
            }
            let carries_value = match inner {
                Value::Null | Value::Bool(false) => false,
                Value::String(text) => !text.is_empty(),
                _ => true,
            };
            let normalized_key = normalize_key(key);
            if carries_value && is_sensitive_key_name(&normalized_key, true) {
                return true;
            }
            json_violates(inner, depth + 1, is_endpoint_key_name(&normalized_key))
        }),
        _ => false,
    }
}

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-'
}

fn contains_unapproved_url(text: &str) -> bool {
    for (separator_index, _) in text.match_indices("://") {
        let scheme_start = text[..separator_index]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-'))
            .map(|position| position + 1)
            .unwrap_or(0);
        let scheme = text[scheme_start..separator_index].to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return true;
        }

        let after_separator = &text[separator_index + 3..];
        let authority_end = after_separator
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '/' | '?' | '#' | '\\' | '"' | '\'' | '<' | '>')
            })
            .unwrap_or(after_separator.len());
        let authority = &after_separator[..authority_end];
        if authority.contains('@') || authority.starts_with('[') {
            return true;
        }
        let host = authority
            .split(':')
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !SYNTHETIC_HOSTS.contains(&host.as_str()) {
            return true;
        }
    }
    false
}

fn is_non_public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
}

fn is_non_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    address.is_loopback()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || address.to_ipv4_mapped().is_some_and(is_non_public_ipv4)
}

fn contains_non_public_ipv6(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.'))
        .filter(|chunk| chunk.matches(':').count() >= 2)
        .any(|chunk| {
            [chunk, chunk.trim_end_matches(':')]
                .iter()
                .filter_map(|candidate| candidate.parse::<Ipv6Addr>().ok())
                .any(is_non_public_ipv6)
        })
}

fn contains_private_network_address(text: &str) -> bool {
    let has_non_public_ipv4 = text
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|chunk| chunk.trim_matches('.'))
        .filter(|chunk| chunk.matches('.').count() == 3)
        .filter_map(|chunk| chunk.parse::<Ipv4Addr>().ok())
        .any(is_non_public_ipv4);
    if has_non_public_ipv4 {
        return true;
    }

    contains_non_public_ipv6(text)
}

/// True if the text names `localhost` or a hostname under a private-network suffix.
fn contains_private_hostname(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
        .map(|chunk| chunk.trim_matches('.').to_ascii_lowercase())
        .any(|host| {
            host == "localhost"
                || PRIVATE_HOST_SUFFIXES
                    .iter()
                    .any(|suffix| host.len() > suffix.len() && host.ends_with(suffix))
        })
}

fn contains_database_or_private_path(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || "\"'(),;<>[]{}=|".contains(c))
        .map(|token| token.trim_end_matches(['.', ':']))
        .filter(|token| !token.is_empty())
        .any(|token| {
            let lowered = token.to_ascii_lowercase();
            let bytes = lowered.as_bytes();
            let windows_drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/');
            windows_drive
                || lowered.starts_with("sqlite:")
                || lowered.starts_with("file:")
                || DATABASE_EXTENSIONS
                    .iter()
                    .any(|extension| lowered.ends_with(extension))
                || PRIVATE_PATH_PREFIXES
                    .iter()
                    .any(|prefix| lowered.starts_with(prefix))
        })
}

struct Word<'a> {
    text: &'a str,
    end: usize,
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut collected = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        match (is_word_char(character), start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                collected.push(Word {
                    text: &text[begin..index],
                    end: index,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        collected.push(Word {
            text: &text[begin..],
            end: text.len(),
        });
    }
    collected
}

fn has_known_secret_prefix(word: &str) -> bool {
    let long_enough_after = |prefix: &str, minimum_remaining: usize| {
        word.starts_with(prefix) && word.len() >= prefix.len() + minimum_remaining
    };
    long_enough_after("sk-", 3)
        || long_enough_after("AIza", 16)
        || long_enough_after("sk_", 8)
        || long_enough_after("ghp_", 8)
        || long_enough_after("gho_", 8)
        || long_enough_after("ghs_", 8)
        || long_enough_after("ghu_", 8)
        || long_enough_after("github_pat_", 8)
        || long_enough_after("xoxb-", 6)
        || long_enough_after("xoxp-", 6)
        || long_enough_after("xoxa-", 6)
        || long_enough_after("eyJ", 13)
        || (word.starts_with("AKIA")
            && word.len() == 20
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
}

fn looks_like_credential_value(value: &str) -> bool {
    value.len() >= 6 && value.chars().any(|c| !c.is_ascii_lowercase())
}

fn contains_secret_material(text: &str) -> bool {
    let tokens = words(text);
    for word in &tokens {
        if has_known_secret_prefix(word.text) {
            return true;
        }

        let lowered = word.text.to_ascii_lowercase();
        if lowered == "bearer" || lowered == "basic" {
            let following = text[word.end..].trim_start_matches([' ', '\t']);
            let credential: String = following
                .chars()
                .take_while(|c| is_word_char(*c) || matches!(c, '.' | '~' | '+' | '/' | '='))
                .collect();
            if looks_like_credential_value(&credential) {
                return true;
            }
        }

        let normalized = normalize_key(word.text);
        let sensitive_key = is_sensitive_key_name(&normalized, true);
        let endpoint_key = is_endpoint_key_name(&normalized);
        if !sensitive_key && !endpoint_key {
            continue;
        }
        let remainder = &text[word.end..];
        let after_key = remainder.trim_start_matches(['"', '\'', '\\']);
        let had_closing_quote = after_key.len() != remainder.len();
        let after_key = after_key.trim_start_matches([' ', '\t']);
        let Some(separator) = after_key.chars().next() else {
            continue;
        };
        if separator != '=' && separator != ':' {
            continue;
        }
        let value = after_key[1..].trim_start_matches([' ', '\t', '"', '\'', '\\']);
        let has_value = value
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace() && !",;&}])".contains(c));
        let value_word: String = value.chars().take_while(|c| is_word_char(*c)).collect();
        if !has_value || value_word == "null" {
            continue;
        }
        let bare_colon = separator == ':' && !had_closing_quote && !is_header_key_name(&normalized);
        let value_token: String = value
            .chars()
            .take_while(|c| !c.is_whitespace() && !",;&}])\"'".contains(*c))
            .collect();
        let value_token = value_token.trim_end_matches(['.', '!', '?']);
        if sensitive_key {
            // Only bare `key: value` prose with a placeholder value, such as "password: required",
            // escapes; header keys and assignments are credentials for any value.
            if !bare_colon || !is_placeholder_value(value_token) {
                return true;
            }
            continue;
        }
        // Endpoint keys must name an approved synthetic host. A bare `key:` followed by plain
        // prose ("server: unavailable") is not an endpoint value.
        if !is_synthetic_endpoint(value_token)
            && !is_placeholder_value(value_token)
            && (!bare_colon || looks_like_host(value_token))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_userinfo_and_private_or_unlisted_hosts() {
        for value in [
            "https://user:hunter2@private.internal/v1",
            "http://user:pass@example.com",
            "https://inference.corp.example.net/v1",
            "https://private.example.test/inference",
            "http://10.1.2.3:8080/v1",
            "http://172.20.0.5/x",
            "http://127.0.0.1:9000",
            "http://169.254.169.254/latest",
            "http://localhost:8080/api",
            "https://api.openai.com/v1/chat",
            "postgres://db.internal/prod",
            "see 192.168.1.20 for details",
            "connect to localhost:5432",
        ] {
            assert!(is_secret_or_endpoint(value), "should reject {value}");
        }
    }

    #[test]
    fn accepts_synthetic_hosts_and_public_numbers() {
        for value in [
            "https://example.com/doc",
            "http://www.example.org/a?b=c",
            "version 1.2.3.4 shipped",
            "date 2026-10-08T12:00:00Z",
            "example.com is the sample host",
        ] {
            assert!(!is_secret_or_endpoint(value), "should accept {value}");
        }
    }

    #[test]
    fn rejects_credential_shapes() {
        for value in [
            "Authorization: Bearer sk-live-abc123",
            "error: Bearer sk-secret-token",
            "Basic dXNlcjpwYXNz",
            "config: api_key=sk-abc123",
            "x-api-key: abc123canary",
            "{\"api_key\":\"canary123\"}",
            "{\\\"api_key\\\": \\\"canary123\\\"}",
            "password=synthetic-canary",
            "client_secret=abc",
            "sk-xyz",
            "ghp_abcdefghijklmnop",
            &["AKIA", "ABCDEFGHIJKLMNOP"].concat(),
            "eyJhbGciOiJIUzI1NiJ9abc",
            &["tok", "en=", "abcdef123456"].concat(),
            &["api_", "token=", "abcdef123456"].concat(),
            "auth=abcdef",
            "password: hunter2",
            "Password: Tr0ub4dor",
            "client_secret: abc123",
            "Cookie: session=abcdef123",
            "Set-Cookie: id=a1b2c3d4",
            "token: Zx9QpL2mN",
            "AIzaSyA1234567890abcdefghij",
            "connect to [::1]:8080",
            "peer fe80::1ff:fe23:4567:890a",
            "peer fd00::12",
        ] {
            assert!(is_secret_or_endpoint(value), "should reject {value}");
        }
    }

    #[test]
    fn accepts_ordinary_failure_and_task_text() {
        for value in [
            "max output tokens exceeded",
            "monkey keystone not found",
            "failed to connect to remote API",
            "Remind me to finish the task-list review tomorrow",
            "provider returned disk-full error",
            "risk-averse and ask-first policy",
            "basic functionality is broken",
            "Bearer tokens are described in the doc",
            "password: required",
            "timeout after 30s",
            "the api key was rotated",
            "token: expired.",
            "token limit reached, tokens_used 42",
            "rust path std::fmt and ratio 12:00:00",
            "peer 2001:db8::1 is documentation",
        ] {
            assert!(!is_secret_or_endpoint(value), "should accept {value}");
        }
    }

    #[test]
    fn rejects_free_text_endpoints_private_hostnames_and_lowercase_credentials() {
        for value in [
            "endpoint=inference.internal",
            "diagnostic endpoint=inference.internal",
            "url=inference.internal",
            "base_url=inference",
            "host: db.corp.local",
            "endpoint: localhost",
            "server: 10.0.0.8",
            "connect to db.internal",
            "localhost",
            "fallback to printer.lan now",
            "router.home.arpa",
            "Cookie: abcdef",
            "Set-Cookie: sessionid",
            "token: abcdefgh",
            "secret: opensesame",
            "credentials: abcdefgh",
            "password: hunter",
        ] {
            assert!(is_secret_or_endpoint(value), "should reject {value}");
        }
    }

    #[test]
    fn accepts_endpoint_prose_and_placeholder_values() {
        for value in [
            "server: unavailable",
            "url: https://example.com/a",
            "endpoint=example.com",
            "host: n/a",
            "password: required",
            "token: expired.",
            "secret: redacted",
            "the local host was slow",
            "curl: (7) failed to connect",
            "release notes.local-first design",
        ] {
            assert!(!is_secret_or_endpoint(value), "should accept {value}");
        }
    }

    #[test]
    fn rejects_database_and_private_path_inputs() {
        for value in [
            "/var/mobile/ohand/production.sqlite",
            "backed up /var/mobile/ohand/production.db",
            "corpus.sqlite3",
            "C:\\Users\\me\\ohand.db",
            "/Users/me/Library/ohand/captures",
            "~/ohand/notes",
            "sqlite:///tmp/x",
            "file:prod.db",
        ] {
            assert!(is_secret_or_endpoint(value), "should reject {value}");
        }
    }

    #[test]
    fn json_walker_checks_keys_values_and_nesting() {
        assert!(json_is_secret_or_endpoint(
            &json!({"endpoint": "https://inference.corp.example.net/v1"})
        ));
        assert!(json_is_secret_or_endpoint(
            &json!({"url": "http://10.1.2.3:8080/v1"})
        ));
        assert!(json_is_secret_or_endpoint(
            &json!({"headers": {"x-api-key": "abc123canary"}})
        ));
        assert!(json_is_secret_or_endpoint(&json!({"API-Key": "x"})));
        assert!(json_is_secret_or_endpoint(&json!({"token": "x"})));
        assert!(json_is_secret_or_endpoint(
            &json!({"apiToken": "abcdef123"})
        ));
        assert!(json_is_secret_or_endpoint(&json!({"note": "token=abcdef"})));
        assert!(json_is_secret_or_endpoint(
            &json!({"api_token": "abcdef123"})
        ));
        for endpoint in [
            "localhost",
            "::1",
            "inference.internal",
            "[::1]:8080",
            "10.0.0.1",
        ] {
            assert!(
                json_is_secret_or_endpoint(&json!({"endpoint": endpoint})),
                "should reject endpoint {endpoint}"
            );
            assert!(json_is_secret_or_endpoint(
                &json!({"hosts": {"server": [endpoint]}})
            ));
        }
        assert!(json_is_secret_or_endpoint(&json!([{"a": ["sk-abc"]}])));
        assert!(json_is_secret_or_endpoint(
            &json!({"note": "diagnostic endpoint=inference.internal"})
        ));
        assert!(json_is_secret_or_endpoint(&json!([
            "endpoint=inference.internal"
        ])));
        assert!(json_is_secret_or_endpoint(
            &json!({"nested": "{\"password\": \"x\"}"})
        ));
    }

    #[test]
    fn json_walker_accepts_normal_output() {
        assert!(!json_is_secret_or_endpoint(
            &json!({"result": "ok", "tokens_used": 42, "max_tokens": 100, "password": null})
        ));
        assert!(!json_is_secret_or_endpoint(
            &json!({"url": "https://example.com/a", "tasks": ["task-list", "disk-check"]})
        ));
    }

    #[test]
    fn json_walker_accepts_synthetic_endpoints_and_empty_values() {
        assert!(!json_is_secret_or_endpoint(
            &json!({"endpoint": "https://example.com/v1", "host": "example.org", "url": ""})
        ));
        assert!(!json_is_secret_or_endpoint(
            &json!({"max_tokens": 100, "tokens_used": 42, "token": null, "cookie": ""})
        ));
    }

    #[test]
    fn json_walker_rejects_excessive_depth() {
        let mut value = json!("leaf");
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            value = json!([value]);
        }
        assert!(json_is_secret_or_endpoint(&value));
    }
}
