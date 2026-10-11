//! Provider adapters the benchmark registers beyond the hosted protocols.
//!
//! Every adapter here is the production adapter from core over the one host transport; none
//! carries its own prompt, request shape or response parser.

pub mod self_hosted;

// The transport tests own this fixture server privately; it is included by path here rather than
// widening its visibility outside this crate's tests.
#[cfg(test)]
#[path = "../transport/fixture.rs"]
#[allow(dead_code, clippy::duplicate_mod)]
mod fixture;
