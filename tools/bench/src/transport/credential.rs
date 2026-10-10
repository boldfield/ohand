//! Out-of-band credential resolution for the developer-only host transport.
//!
//! Secret bytes exist only in this crate, only between resolution and the end of one dispatch,
//! and are never printed: [`SecretValue`] has no `Display`, a redacting `Debug`, and zeroizes
//! its buffer on drop. Resolution happens per dispatch; nothing here caches a value.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::sync::Arc;

use thiserror::Error;
use zeroize::Zeroizing;

/// A resolved secret. Only printable, non-space ASCII is accepted so it can never inject
/// header lines or hide inside a differently framed value.
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<SecretValue, CredentialError> {
        let bytes = Zeroizing::new(bytes);
        if bytes.is_empty() || !bytes.iter().all(|byte| (0x21..=0x7e).contains(byte)) {
            return Err(CredentialError::Malformed);
        }
        Ok(SecretValue(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([redacted])")
    }
}

/// Resolution failures carry no reference, path, variable name or value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialError {
    #[error("credential reference is not configured")]
    NotFound,
    #[error("credential could not be read")]
    Unreadable,
    #[error("credential value is not usable")]
    Malformed,
}

/// Out-of-band lookup of the secret behind an opaque credential reference. Called once per
/// dispatch with exactly the reference the request names; an implementation must not substitute
/// another reference when the requested one is missing.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, reference: &str) -> Result<SecretValue, CredentialError>;
}

type EnvLookup = dyn Fn(&OsStr) -> Option<OsString> + Send + Sync;

/// Reads secrets from process environment variables named by an explicit, non-secret mapping
/// from credential reference to variable name. An unmapped reference is `NotFound`. `Debug`
/// prints only the mapping count: references and variable names name private configuration.
#[derive(Clone)]
pub struct EnvCredentialResolver {
    variables_by_reference: BTreeMap<String, String>,
    lookup: Arc<EnvLookup>,
}

impl fmt::Debug for EnvCredentialResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvCredentialResolver")
            .field("reference_count", &self.variables_by_reference.len())
            .finish_non_exhaustive()
    }
}

impl Default for EnvCredentialResolver {
    fn default() -> EnvCredentialResolver {
        EnvCredentialResolver::with_lookup(|name| std::env::var_os(name))
    }
}

impl EnvCredentialResolver {
    pub fn new() -> EnvCredentialResolver {
        EnvCredentialResolver::default()
    }

    /// Uses `lookup` instead of the process environment, so tests never mutate shared state.
    pub fn with_lookup(
        lookup: impl Fn(&OsStr) -> Option<OsString> + Send + Sync + 'static,
    ) -> EnvCredentialResolver {
        EnvCredentialResolver {
            variables_by_reference: BTreeMap::new(),
            lookup: Arc::new(lookup),
        }
    }

    pub fn map_reference(
        mut self,
        reference: impl Into<String>,
        variable_name: impl Into<String>,
    ) -> EnvCredentialResolver {
        self.variables_by_reference
            .insert(reference.into(), variable_name.into());
        self
    }
}

impl CredentialResolver for EnvCredentialResolver {
    fn resolve(&self, reference: &str) -> Result<SecretValue, CredentialError> {
        let variable_name = self
            .variables_by_reference
            .get(reference)
            .ok_or(CredentialError::NotFound)?;
        let raw = (self.lookup)(OsStr::new(variable_name)).ok_or(CredentialError::NotFound)?;
        let text = raw.into_string().map_err(|rejected| {
            drop(Zeroizing::new(rejected.into_encoded_bytes()));
            CredentialError::Unreadable
        })?;
        SecretValue::from_bytes(text.into_bytes())
    }
}
