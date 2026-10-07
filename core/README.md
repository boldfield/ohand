# Oh And Core

The Rust core domain logic and SQLite-backed storage for the Oh And application.

## Workspace and Build

The core workspace is defined in `Cargo.toml` (workspace root). Dependencies are pinned in `Cargo.lock` and use a deterministic Rust toolchain (see `rust-toolchain.toml`).

### Build and Testing Commands

Run from the repository root:

- `make check` — Compile and validate: contract checks, `cargo check`, format check, and clippy lint.
- `make test` — Run contract tests and `cargo test`.
- `make cargo-build` — Build release binary with optimizations and LTO.

All commands use `--locked` to ensure reproducible builds against pinned dependencies.

### Toolchain and Reproducibility

- Rust toolchain is pinned to `1.99.0` in `rust-toolchain.toml`.
- `Cargo.lock` is committed and kept up-to-date.
- All cargo invocations use `--locked` to prevent dependency drift.
- Dependencies are minimal and focused on domain logic: SQLite, serialization (serde), UUIDs, chrono for datetime, and error handling.

## Module Organization

Modules are organized by responsibility according to the M1 architecture contracts (`docs/architecture/m1-contracts.md`):

- `src/store/` — Durable storage: schema, captures, user corrections and lifecycle events.
- `src/domain/` — Authoritative item state projection and status machines.
- `src/ingress/` — Native capture handoff and import contract.
- `src/time/` — Date/time resolution, timezone and locale handling.
- `src/interpretation/` — Proposal contracts, interpretation dispatch, and result application.
- `src/providers/` — Provider protocol adapters and normalized interfaces.
- `src/privacy/` — Route authorization and destination validation.
- `src/jobs/` — Durable job queue, lease management, and orchestration.
- `src/retrieval/` — Full-text indexing and query execution.
- `src/reminders/` — Reminder desired state, reconciliation, and orchestration.
- `src/suggestions/` — Eligibility scoring, scheduling, and preview permission enforcement.
- `src/review/` — Optional shadow review services (diagnostic only).
- `src/lifecycle/` — Deletion intent, retention policy, and cleanup ordering.
- `src/export/` — Versioned export and source-safe serialization.
- `src/ffi/` — Typed bindings, ABI, thread safety, and error conversion.
- `src/metrics/` — Content-free latency and reliability metrics.

## Testing

The smoke test (`tests/smoke_test.rs`) verifies:
- Core crate compiles and exports its public interface.
- Key dependencies (UUID, chrono, serde) serialize and deserialize correctly.
- The workspace structure supports the intended serialization contracts.

## Initial Dependencies

**M1 module inclusion strategy:**
- **chrono** — Primary datetime handling, integrated with rusqlite. Core uses chrono for capture instants, timezone handling, and reminders.
- **serde + serde_json** — Serialization and proposal/job schemas.
- **uuid** — Immutable capture IDs, item IDs, and profile versions.
- **rusqlite** — Embedded SQLite with bundled build and chrono support.
- **anyhow + thiserror** — Error handling and context.

The `time` crate is reserved for I02 (time/timezone module) if specialized parsing becomes needed; core initially uses chrono exclusively.
