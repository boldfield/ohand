//! Optional budgeted shadow review (sampled, diagnostic only).
//!
//! Shadow review is off by default and never required for capture. A case is queued only when
//! the policy is enabled, the case is sampled, the rolling request budget can reserve its
//! allowance, and the capture's route holds a separate stored `review` grant covering the
//! pinned review profile's destinations ([`crate::privacy::routing::authorize_job`]). Each
//! attempt, retries included, is re-checked by [`authorize_shadow_dispatch`] immediately before
//! a request is sent.
//!
//! A sampled case is a `shadow_review` row in the durable job queue, so its provenance (item,
//! revision, request version, pinned review profile, route) and its outcome (pending, reviewed,
//! unreviewed, error) live in the same transactional store as other jobs and follow its
//! revocation, deletion and stale-revision handling. This module only reads items, reminders,
//! routes and grants, and writes only `shadow_review` job rows: a shadow outcome has no path to
//! item, reminder or permission state, and the interpretation apply path refuses shadow jobs.

mod dispatch;
mod policy;
mod record;
mod selection;
#[cfg(test)]
mod tests;

pub use dispatch::{
    authorize_shadow_dispatch, record_shadow_failure, record_shadow_unreviewed,
    ShadowDispatchDecision, ShadowDispatchDenial,
};
pub use policy::{PolicyError, ShadowPolicy, SAMPLE_SCALE};
pub use record::{
    list_shadow_records, load_shadow_record, ShadowOutcome, ShadowRecord, UnreviewedReason,
};
pub use selection::{
    requests_in_use, select_for_shadow_review, ShadowCandidate, ShadowSelection, ShadowSkip,
};
