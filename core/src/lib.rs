//! Oh And core domain logic and SQLite-backed storage.
//!
//! This crate contains the authoritative domain state, interpretation coordination,
//! provider protocol adapters, job orchestration, reminder logic, retrieval, and
//! policy enforcement for the Oh And application.

pub mod store;
pub mod domain;
pub mod ingress;
pub mod time;
pub mod interpretation;
pub mod providers;
pub mod privacy;
pub mod jobs;
pub mod retrieval;
pub mod reminders;
pub mod suggestions;
pub mod review;
pub mod lifecycle;
pub mod export;
pub mod ffi;
pub mod metrics;
