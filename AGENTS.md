# Oh And agent instructions

Read [DESIGN.md](DESIGN.md) before planning or implementing work.

## Interactive coordination

Interactive agents investigate, discuss, and maintain design/specification documents. Application code changes go through Odonian tasks. Direct documentation edits and repository bootstrap mechanics are allowed. Do not implement features or fixes directly unless the maintainer explicitly requests a direct edit.

## Project-specific autonomous delivery

This project explicitly authorizes agent-driven delivery with **no routine human approval or merge gate**. This overrides generic Odonian skill defaults calling for human approval of each design choice, task, milestone, merge, or release. It does not remove independent review, required checks, or real test evidence, and does not change other projects' policies.

- Decompose build/design work into small, single-purpose, Haiku-sized tasks. Set initial `model=haiku` and `escalate=true`.
- Set immutable `agent_merge=true` when creating each implementation task.
- Specify independent Claude and Codex reviewers explicitly. The intended pair is `opus` and `gpt-6.1-sol`; verify the deployed allowlist and worker availability before dispatch, and use supported equivalents if those identifiers change.
- Use the Odonian REST API to create tasks in batches of 2–3. Register a design/feature document first and link every task to its document.
- State intent, scope, dependencies, acceptance criteria, and meaningful validation in prose. Include source pointers once code exists. Keep the dependency graph acyclic and serialize overlapping file edits.
- Promote work by milestone readiness. The coordinator can advance milestones when exit evidence is satisfied; no routine human checkpoint is required.
- Implementers and reviewers do not self-merge. Odonian's separate merge worker lands work after all required independent reviews and checks pass.
- Repair, escalate, or decompose failed work. Do not lower acceptance criteria, remove failing checks, or create an endless approval loop to force completion.
- Verify live forge access, workers, reviewer models, merger availability, and repository checks before claiming unattended delivery works. Task policy is set per task; this document alone is not service configuration.
- Avoid opting into an Odonian feature whose contract requires human merge, such as the currently documented research continuation-manifest path. Use supported ordinary task coordination instead; do not bypass checks or rewrite Odonian as part of this app.

Credentials, account enrollment/legal actions, and genuine device experiences are external inputs. Continue independent work when one is missing. Never fabricate a device test, successful deployment, or supported provider capability.

## Implementation and review

- Use descriptive variable names and keep provider protocols separate from domain behavior.
- Preserve raw captures and explicit corrections. Model output cannot silently overwrite authoritative intent.
- Capture must remain durable during network, provider, and reviewer outages.
- Test retry/idempotency, conflicts, reminder states, migrations/restore, deletion, and processing-boundary enforcement where a change affects them.
- Run checks appropriate to the task. There is no application or canonical build/test command at bootstrap; establish those through implementation tasks and update these instructions. Never claim a placeholder command passed.
- Use synthetic fixtures in this public repository. Do not commit secrets, recordings, personal captures, private conversations, signing material, or internal service endpoints.
- Required CI and independent review remain gates even though human approval is not one. Use standard hosted runners and bounded artifact retention; paid capacity requires separate authorization.
- Runtime adversarial review follows DESIGN.md and is not a substitute for independent code review.

## Required checks for Linux Odonian workers

This section documents the canonical build, test, and lint checks that must pass on all PRs affecting the Rust core. Each check runs on a clean checkout with pinned dependencies (Cargo.lock) and fails on test/lint failure. No placeholder success conditions are acceptable.

Rust core checks are executed via the Makefile:

- `make check` — Validate contract correctness, compile, format, and lint:
  - `contract-check` — Python contract validation
  - `cargo-check --all --all-targets --locked` — Compilation with locked dependencies
  - `cargo fmt --all -- --check` — Code format validation (no in-place changes)
  - `cargo clippy --all-targets --locked -- -D warnings` — Lint pass with warnings-as-errors

- `make test` — Run contract and Rust test suites:
  - `contract-test` — Python contract tests
  - `cargo test --all --locked` — Full Rust test suite with locked dependencies

These checks are wired into `.github/workflows/core.yml` on Linux runners (ubuntu-latest) via GitHub Actions. The workflow runs on all pull requests to main and validates that the change compiles, passes tests, and meets lint standards with locked dependencies. Evidence collection is separate from CI: device/simulator results, live trial data, and provider integration tests are distinct artifacts and do not gate the Linux checks.

The first repository commit contains documentation and license material only. It is a bootstrap operation, not an application implementation or proof of completed milestones.
