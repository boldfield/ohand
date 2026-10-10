//! Destination and credential-use approval for the host transport.
//!
//! Approval is an explicit grant list owned by the transport's constructor. Experiment or
//! profile selection never adds to it, and a credential reference is usable only at the origin
//! it was granted for.

use url::Url;

use super::error::HostTransportError;

/// `https://host:port` in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin(String);

fn origin_of(url: &Url) -> Origin {
    let host = url.host_str().unwrap_or_default();
    let port = url.port_or_known_default().unwrap_or(443);
    Origin(format!("https://{host}:{port}"))
}

/// Parses a destination that is acceptable to send to at all: https, a host, no userinfo.
/// The returned URL is the exact one the transport requests, so the checked and the contacted
/// destination cannot differ.
pub(crate) fn parse_https_url(text: &str) -> Result<Url, HostTransportError> {
    let url = Url::parse(text).map_err(|_| HostTransportError::InvalidRequest)?;
    if url.scheme() != "https" {
        return Err(HostTransportError::InvalidRequest);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(HostTransportError::InvalidRequest);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HostTransportError::InvalidRequest);
    }
    Ok(url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    origin: Origin,
    credential_reference: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DestinationPolicy {
    grants: Vec<Grant>,
}

impl DestinationPolicy {
    pub fn new() -> DestinationPolicy {
        DestinationPolicy::default()
    }

    /// Approves requests without a credential to the origin of `url`.
    pub fn approve_origin(mut self, url: &str) -> Result<DestinationPolicy, HostTransportError> {
        let origin = origin_of(&parse_https_url(url)?);
        self.grants.push(Grant {
            origin,
            credential_reference: None,
        });
        Ok(self)
    }

    /// Approves sending the secret behind `credential_reference` to the origin of `url`, and
    /// only there. The same origin is also approved for requests without a credential.
    pub fn approve_credential(
        mut self,
        url: &str,
        credential_reference: &str,
    ) -> Result<DestinationPolicy, HostTransportError> {
        let origin = origin_of(&parse_https_url(url)?);
        self.grants.push(Grant {
            origin,
            credential_reference: Some(credential_reference.to_string()),
        });
        Ok(self)
    }

    pub(crate) fn check(
        &self,
        url: &Url,
        credential_reference: Option<&str>,
    ) -> Result<(), HostTransportError> {
        let origin = origin_of(url);
        let mut origin_approved = false;
        for grant in &self.grants {
            if grant.origin != origin {
                continue;
            }
            origin_approved = true;
            if let Some(reference) = credential_reference {
                if grant.credential_reference.as_deref() == Some(reference) {
                    return Ok(());
                }
            }
        }
        match (origin_approved, credential_reference) {
            (false, _) => Err(HostTransportError::DestinationNotApproved),
            (true, None) => Ok(()),
            (true, Some(_)) => Err(HostTransportError::CredentialNotApproved),
        }
    }
}
