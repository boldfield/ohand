//! Native-boundary exports owned by this module tree.
//!
//! To export an operation, add a file here (for example `ffi/capture.rs`) containing
//! `#[no_mangle] pub extern "C" fn ohand_...` functions, plus the single `pub mod capture;`
//! line below. Nothing else is edited: `core/bindings` links this crate, and `tools/bindings`
//! parses it, so the function reaches both the static library and the generated C header.
//! A module that is not declared here is exported nowhere.
//!
//! Rules enforced by `make check` and `make test` (see `docs/validation/core-binding.md`):
//! exported functions are named `ohand_*`, exported constants `OHAND_*` (other public
//! constants of this crate are never exported), types appear in the header only when an
//! exported function uses them, and no export is gated on the build target.

pub mod audio_retention;
pub mod capture;
pub mod core_handle;
pub mod deletion_cleanup;
pub mod export_snapshot;
pub mod ingress_import;
pub mod metrics;
pub mod provider_profiles;
pub mod provider_transport;
pub mod transcription_attachment;

pub mod job_health;
pub mod job_runner;
pub mod notification_actions;
pub mod notification_reconciliation;
pub mod prompt_schedule;
pub mod reset;
pub mod retrieval;
pub mod route_policy;
pub mod route_provisioning;
