//! Length-preserving removal of a resolved secret from data coming back from the server, so a
//! provider that echoes a credential in an error body or header cannot return it to callers.
//! Masking keeps lengths unchanged so response-size bound decisions are unaffected.

const MASK: u8 = b'*';

/// Masks every occurrence of the secret in its raw and JSON-string-escaped forms. When the data
/// was cut at the size bound, a trailing partial prefix of the secret is masked too.
pub(crate) fn mask_secret(data: &mut [u8], secret: &[u8], truncated: bool) {
    if secret.is_empty() {
        return;
    }
    mask_all(data, secret);
    let escaped = json_escaped(secret);
    if escaped != secret {
        mask_all(data, &escaped);
    }
    if truncated {
        mask_trailing_prefix(data, secret);
        if escaped != secret {
            mask_trailing_prefix(data, &escaped);
        }
    }
}

fn mask_all(data: &mut [u8], needle: &[u8]) {
    if needle.len() > data.len() {
        return;
    }
    let mut start = 0;
    while start + needle.len() <= data.len() {
        if &data[start..start + needle.len()] == needle {
            data[start..start + needle.len()].fill(MASK);
            start += needle.len();
        } else {
            start += 1;
        }
    }
}

fn mask_trailing_prefix(data: &mut [u8], secret: &[u8]) {
    let longest = secret.len().saturating_sub(1).min(data.len());
    for prefix_len in (1..=longest).rev() {
        let tail_start = data.len() - prefix_len;
        if data[tail_start..] == secret[..prefix_len] {
            data[tail_start..].fill(MASK);
            return;
        }
    }
}

fn json_escaped(secret: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(secret.len());
    for &byte in secret {
        match byte {
            b'"' => escaped.extend_from_slice(b"\\\""),
            b'\\' => escaped.extend_from_slice(b"\\\\"),
            other => escaped.push(other),
        }
    }
    escaped
}
