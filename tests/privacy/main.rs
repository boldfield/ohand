//! Outbound destination and credential isolation (T04a). Every test drives the real job runner,
//! interpretation dispatcher, provider adapters and route authorization against a SQLite file
//! whose capture came in through the real foreground ingress import. Only the HTTP transports
//! are scripted, and they record everything they are handed. Synthetic canaries stand in for
//! capture content and for provider-side secrets; a canary must reach a transport only when the
//! route, the pinned profile and the adapter all authorize that exact destination, and a secret
//! canary must never appear in core state or diagnostics. All content is synthetic.
//!
//! Scope: base provider authorization, transport, redirects, and queued or in-flight
//! configuration changes. Optional Spark, shadow-review and preview requirements keep their own
//! canaries with their owners. T04b (private reads, notifications and export) adds modules here.

mod fixtures;
mod outbound_tests;
