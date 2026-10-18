//! Environment-driven configuration. Backends are chosen here, never by code edits.
//!
//! * `OHAND_LIVE_PROFILES`: comma-separated `<protocol>:<model>` specs, protocol `self-hosted`,
//!   `anthropic` or `openai`. Any model name is allowed.
//! * `OHAND_LIVE_CREDENTIAL_ENVS`: comma-separated `<protocol>=<VARIABLE>` pairs naming the
//!   environment variable that holds each hosted protocol's credential. Only names are given on
//!   the command line, never values.
//! * `OHAND_PROVIDER_CONFIG_PATH` (default `/etc/ohand-provider/models.json`), provider key
//!   `ollama`: the self-hosted endpoint and credential, as `live_smoke.rs` reads them.
//! * `OHAND_LIVE_TIMEOUT_SECONDS`: per-request timeout, 1 to 300 (default 300).
//! * `OHAND_LIVE_RATES_FILE`: optional JSON of per-million-token rates, see `evidence.rs`.

use serde_json::Value;
use std::collections::BTreeMap;

pub const PROFILES_ENV_VAR: &str = "OHAND_LIVE_PROFILES";
pub const CREDENTIAL_ENVS_ENV_VAR: &str = "OHAND_LIVE_CREDENTIAL_ENVS";
pub const TIMEOUT_ENV_VAR: &str = "OHAND_LIVE_TIMEOUT_SECONDS";
pub const RATES_ENV_VAR: &str = "OHAND_LIVE_RATES_FILE";
pub const EVIDENCE_DIR_ENV_VAR: &str = "OHAND_LIVE_EVIDENCE_DIR";
pub const PRIVATE_EVIDENCE_DIR_ENV_VAR: &str = "OHAND_PRIVATE_EVIDENCE_DIR";
pub const DEFAULT_PRIVATE_EVIDENCE_DIRECTORY: &str = ".ohand-private-evidence";
const CONFIG_PATH_ENV_VAR: &str = "OHAND_PROVIDER_CONFIG_PATH";
const DEFAULT_CONFIG_PATH: &str = "/etc/ohand-provider/models.json";
const PROVIDER_KEY: &str = "ollama";
pub const MAX_TIMEOUT_SECONDS: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Protocol {
    SelfHosted,
    Anthropic,
    OpenAi,
}

impl Protocol {
    pub fn parse(text: &str) -> Result<Protocol, String> {
        match text {
            "self-hosted" => Ok(Protocol::SelfHosted),
            "anthropic" => Ok(Protocol::Anthropic),
            "openai" => Ok(Protocol::OpenAi),
            _ => Err(format!(
                "unknown protocol {text:?}; use self-hosted, anthropic or openai"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::SelfHosted => "self-hosted",
            Protocol::Anthropic => "anthropic",
            Protocol::OpenAi => "openai",
        }
    }

    /// The key the core's profile configuration path uses for the protocol.
    pub fn draft_key(self) -> &'static str {
        match self {
            Protocol::SelfHosted => "self_hosted",
            Protocol::Anthropic => "anthropic",
            Protocol::OpenAi => "open_ai",
        }
    }

    pub fn is_hosted(self) -> bool {
        self != Protocol::SelfHosted
    }
}

/// One configured backend. `endpoint` is set for self-hosted only; `credential` is the secret,
/// held in memory and never written anywhere.
#[derive(Clone)]
pub struct Backend {
    pub protocol: Protocol,
    pub model: String,
    pub endpoint: Option<String>,
    pub credential: String,
}

impl Backend {
    /// Label used in artifacts; carries no endpoint or credential.
    pub fn label(&self) -> String {
        format!("{}:{}", self.protocol.label(), self.model)
    }
}

pub struct LiveConfig {
    pub backends: Vec<Backend>,
    pub timeout_seconds: u32,
}

pub fn parse_profile_specs(text: &str) -> Result<Vec<(Protocol, String)>, String> {
    let mut specs = Vec::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (protocol, model) = entry
            .split_once(':')
            .ok_or_else(|| format!("profile spec {entry:?} is not <protocol>:<model>"))?;
        if model.trim().is_empty() {
            return Err(format!("profile spec {entry:?} names no model"));
        }
        specs.push((Protocol::parse(protocol.trim())?, model.trim().to_string()));
    }
    if specs.is_empty() {
        return Err("no profile specs given".to_string());
    }
    Ok(specs)
}

pub fn parse_credential_envs(text: &str) -> Result<BTreeMap<Protocol, String>, String> {
    let mut names = BTreeMap::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (protocol, variable) = entry
            .split_once('=')
            .ok_or_else(|| format!("credential entry {entry:?} is not <protocol>=<VARIABLE>"))?;
        names.insert(
            Protocol::parse(protocol.trim())?,
            variable.trim().to_string(),
        );
    }
    Ok(names)
}

pub fn timeout_from(text: Option<&str>) -> Result<u32, String> {
    let Some(text) = text else {
        return Ok(MAX_TIMEOUT_SECONDS);
    };
    match text.trim().parse::<u32>() {
        Ok(seconds) if (1..=MAX_TIMEOUT_SECONDS).contains(&seconds) => Ok(seconds),
        _ => Err(format!(
            "timeout must be 1 to {MAX_TIMEOUT_SECONDS} seconds"
        )),
    }
}

/// The self-hosted endpoint and credential from the provider configuration file.
pub fn load_self_hosted_endpoint() -> Result<(String, String), String> {
    let path =
        std::env::var(CONFIG_PATH_ENV_VAR).unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| "the provider configuration file is not readable".to_string())?;
    let config: Value = serde_json::from_str(&raw)
        .map_err(|_| "the provider configuration file is not JSON".to_string())?;
    let provider = &config["providers"][PROVIDER_KEY];
    let field = |key: &str| {
        provider[key]
            .as_str()
            .map(|value| value.trim().to_string())
            .ok_or_else(|| format!("the provider configuration lacks {key}"))
    };
    Ok((field("baseUrl")?, field("apiKey")?))
}

impl LiveConfig {
    pub fn from_env() -> Result<LiveConfig, String> {
        let specs = parse_profile_specs(
            &std::env::var(PROFILES_ENV_VAR)
                .map_err(|_| format!("{PROFILES_ENV_VAR} must list the profile specs"))?,
        )?;
        let credential_names =
            parse_credential_envs(&std::env::var(CREDENTIAL_ENVS_ENV_VAR).unwrap_or_default())?;
        let mut backends = Vec::new();
        for (protocol, model) in specs {
            let (endpoint, credential) = if protocol.is_hosted() {
                let variable = credential_names.get(&protocol).ok_or_else(|| {
                    format!(
                        "{CREDENTIAL_ENVS_ENV_VAR} must name the variable for {}",
                        protocol.label()
                    )
                })?;
                let value = std::env::var(variable)
                    .map_err(|_| format!("the variable {variable} is not set"))?;
                (None, value.trim().to_string())
            } else {
                let (endpoint, credential) = load_self_hosted_endpoint()?;
                (Some(endpoint), credential)
            };
            backends.push(Backend {
                protocol,
                model,
                endpoint,
                credential,
            });
        }
        Ok(LiveConfig {
            backends,
            timeout_seconds: timeout_from(std::env::var(TIMEOUT_ENV_VAR).ok().as_deref())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_reject_malformed_entries() {
        let specs = parse_profile_specs("self-hosted:a-model, anthropic:claude-x").unwrap();
        assert_eq!(specs[0], (Protocol::SelfHosted, "a-model".to_string()));
        assert_eq!(specs[1].0, Protocol::Anthropic);
        for bad in ["", "anthropic", "gemini:m", "openai:"] {
            assert!(parse_profile_specs(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn credential_entries_name_variables_not_values() {
        let names = parse_credential_envs("anthropic=A_KEY,openai=O_KEY").unwrap();
        assert_eq!(names[&Protocol::OpenAi], "O_KEY");
        assert!(parse_credential_envs("anthropic").is_err());
    }

    #[test]
    fn timeout_is_bounded_by_three_hundred_seconds() {
        assert_eq!(timeout_from(None).unwrap(), 300);
        assert_eq!(timeout_from(Some("45")).unwrap(), 45);
        for bad in ["0", "301", "soon"] {
            assert!(timeout_from(Some(bad)).is_err());
        }
    }
}
