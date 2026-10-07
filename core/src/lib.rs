//! Oh And core domain logic and SQLite-backed storage.
//!
//! This crate contains the authoritative domain state, interpretation coordination,
//! provider protocol adapters, job orchestration, reminder logic, retrieval, and
//! policy enforcement for the Oh And application.

/// Core crate version marker for schema and API contracts.
pub const CORE_VERSION: &str = "0.1.0-m1";

pub mod domain;
pub mod export;
pub mod ffi;
pub mod ingress;
pub mod interpretation;
pub mod jobs;
pub mod lifecycle;
pub mod metrics;
pub mod privacy;
pub mod providers;
pub mod reminders;
pub mod retrieval;
pub mod review;
pub mod store;
pub mod suggestions;
pub mod time;
