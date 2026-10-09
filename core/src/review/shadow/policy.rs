use sha2::{Digest, Sha256};
use thiserror::Error;

/// Sampling resolution: `sample_per_mille` is a share out of this many.
pub const SAMPLE_SCALE: u16 = 1000;

const SAMPLING_DOMAIN: &[u8] = b"ohand-shadow-sample-v1";

/// Opt-in sampling and request-budget limits for shadow review. The default is off with a zero
/// budget, so a fresh install, a missing configuration and an unparsed one all dispatch nothing.
///
/// The budget unit is one provider request. Selecting a case reserves `max_attempts_per_sample`
/// requests for it, held until the case ends; retries spend that reservation and never add to
/// it, and requests a case did not use are released when it ends. Requests a case did use count
/// against the window containing its latest dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowPolicy {
    pub enabled: bool,
    /// Share of eligible cases sampled, out of [`SAMPLE_SCALE`].
    pub sample_per_mille: u16,
    /// Length of the rolling budget window ending at the selection instant.
    pub window_seconds: i64,
    /// Provider requests that may be reserved (while a case is open) or spent within one window.
    pub max_requests_per_window: u32,
    /// Most provider requests (first attempt plus retries) one sampled case may use.
    pub max_attempts_per_sample: u32,
}

impl Default for ShadowPolicy {
    fn default() -> Self {
        ShadowPolicy {
            enabled: false,
            sample_per_mille: 0,
            window_seconds: 24 * 60 * 60,
            max_requests_per_window: 0,
            max_attempts_per_sample: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PolicyError {
    #[error("sample share exceeds {SAMPLE_SCALE} per mille")]
    SampleShareTooLarge,
    #[error("budget window must be positive")]
    NonPositiveWindow,
    #[error("a sampled case must be allowed at least one attempt")]
    NoAttemptsPerSample,
}

impl ShadowPolicy {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.sample_per_mille > SAMPLE_SCALE {
            return Err(PolicyError::SampleShareTooLarge);
        }
        if self.window_seconds <= 0 {
            return Err(PolicyError::NonPositiveWindow);
        }
        if self.max_attempts_per_sample == 0 {
            return Err(PolicyError::NoAttemptsPerSample);
        }
        Ok(())
    }

    /// Deterministic sampling decision for one case. It depends only on the case identity, so
    /// repeating a selection never flips the answer and needs no stored random state.
    pub fn samples(
        &self,
        item_id: &str,
        source_revision: i32,
        request_version: &str,
        review_profile_version: &str,
    ) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(SAMPLING_DOMAIN);
        for part in [
            item_id,
            &source_revision.to_string(),
            request_version,
            review_profile_version,
        ] {
            hasher.update([0u8]);
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let bucket = u64::from_be_bytes(digest[..8].try_into().expect("digest has 32 bytes"))
            % u64::from(SAMPLE_SCALE);
        bucket < u64::from(self.sample_per_mille)
    }
}
