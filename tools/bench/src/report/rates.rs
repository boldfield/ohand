//! Caller-supplied, versioned price rates. The tool carries no price list and looks nothing up:
//! without a rates file every cost is unknown, and with one every figure names the rates version
//! it came from and says it is an estimate.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use evaluation::comparison::Label;
use serde::Deserialize;

use super::ReportError;

const MAX_RATES_BYTES: u64 = 64 * 1024;
const MAX_PROFILES: usize = 16;
/// Ten thousand currency units per million tokens: far above any real price, low enough that
/// no token count the journal can hold overflows the arithmetic.
pub const MAX_MICROS_PER_MILLION_TOKENS: u64 = 10_000_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatesFile {
    rates_version: String,
    currency: String,
    profiles: Vec<ProfileRateFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRateFile {
    profile_version: String,
    input_micros_per_million_tokens: u64,
    output_micros_per_million_tokens: u64,
}

/// What one profile version costs per million tokens, in millionths of the currency unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRate {
    pub profile_version: Label,
    pub input_micros_per_million_tokens: u64,
    pub output_micros_per_million_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostRates {
    pub rates_version: Label,
    pub currency: Label,
    profiles: Vec<ProfileRate>,
}

impl CostRates {
    pub fn from_bytes(bytes: &[u8]) -> Result<CostRates, ReportError> {
        let file: RatesFile = serde_json::from_slice(bytes).map_err(|_| {
            ReportError(
                "the rates file is not the expected JSON: rates_version, currency and profiles"
                    .to_string(),
            )
        })?;
        let invalid_label = |field: &str| {
            ReportError(format!(
                "the rates file {field} is not a plain label (letters, digits, '.', '_' and '-')"
            ))
        };
        let rates_version =
            Label::new(&file.rates_version).map_err(|_| invalid_label("rates_version"))?;
        let currency = Label::new(&file.currency).map_err(|_| invalid_label("currency"))?;
        if file.profiles.is_empty() || file.profiles.len() > MAX_PROFILES {
            return Err(ReportError(format!(
                "the rates file lists 1 to {MAX_PROFILES} profiles"
            )));
        }
        let mut seen = HashSet::new();
        let mut profiles = Vec::new();
        for entry in &file.profiles {
            let profile_version =
                Label::new(&entry.profile_version).map_err(|_| invalid_label("profile_version"))?;
            if !seen.insert(entry.profile_version.clone()) {
                return Err(ReportError(
                    "the rates file lists a profile version twice".to_string(),
                ));
            }
            for rate in [
                entry.input_micros_per_million_tokens,
                entry.output_micros_per_million_tokens,
            ] {
                if rate > MAX_MICROS_PER_MILLION_TOKENS {
                    return Err(ReportError(format!(
                        "a rate exceeds the supported maximum of {MAX_MICROS_PER_MILLION_TOKENS} \
                         micros per million tokens"
                    )));
                }
            }
            profiles.push(ProfileRate {
                profile_version,
                input_micros_per_million_tokens: entry.input_micros_per_million_tokens,
                output_micros_per_million_tokens: entry.output_micros_per_million_tokens,
            });
        }
        Ok(CostRates {
            rates_version,
            currency,
            profiles,
        })
    }

    pub fn load(path: &Path) -> Result<CostRates, ReportError> {
        let unreadable = || ReportError("the rates file could not be read".to_string());
        let file = std::fs::File::open(path).map_err(|_| unreadable())?;
        let mut bytes = Vec::new();
        file.take(MAX_RATES_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unreadable())?;
        if bytes.len() as u64 > MAX_RATES_BYTES {
            return Err(ReportError(format!(
                "the rates file is larger than {MAX_RATES_BYTES} bytes"
            )));
        }
        CostRates::from_bytes(&bytes)
    }

    pub fn for_profile(&self, profile_version: &str) -> Option<&ProfileRate> {
        self.profiles
            .iter()
            .find(|rate| rate.profile_version.as_str() == profile_version)
    }
}
