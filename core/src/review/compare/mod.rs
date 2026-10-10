//! Pure derivation of independent, authorized, comparable request pairs.
//!
//! Two interpretation profiles are compared on the same immutable input. Both arms receive
//! the same instructions and the same normalized context (source text, revision, time
//! context), derived once from a [`FrozenComparisonSource`]; neither carries the profile
//! identity of the other arm or any answer or rationale from it, because no type here accepts
//! one. Each arm must hold its own current [`Authorization`](crate::privacy::routing::Authorization)
//! for text interpretation on the source's route: an experiment never grants a destination
//! permission, a revoked or unverified profile is rejected, and the `review` grant is not
//! accepted as a substitute.
//!
//! The module touches no store and holds no apply capability. Authorization is a caller-supplied
//! decision that must be obtained immediately before dispatch, and a pair is evidence about
//! configured profiles only: [`Comparability`] lists known configuration differences and
//! settings left to unknown provider defaults, and agreement between the arms is never accuracy.

mod arm;
mod pair;
mod source;
#[cfg(test)]
mod tests;

pub use arm::{ArmError, ArmInput, ArmLabel};
pub use pair::{
    check_comparable, derive_pair, Comparability, ComparisonLimitation, ConfigurationDifference,
    PairedArm, PairedRequests, RequestMismatch,
};
pub use source::{ComparisonError, FrozenComparisonSource};
