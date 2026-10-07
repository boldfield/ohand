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
- Run checks appropriate to the task. The canonical build and test commands for the Rust core are `make check` and `make test`; these are documented in the "Required checks for Linux Odonian workers" section below. Never claim a placeholder command passed.
- Use synthetic fixtures in this public repository. Do not commit secrets, recordings, personal captures, private conversations, signing material, or internal service endpoints.
- Required CI and independent review remain gates even though human approval is not one. Use standard hosted runners and bounded artifact retention; paid capacity requires separate authorization.
- Runtime adversarial review follows DESIGN.md and is not a substitute for independent code review.

## Required checks for Linux Odonian workers

This section documents the canonical build, test, and lint checks that must pass on all PRs affecting the Rust core. Each check runs on a clean checkout with pinned dependencies (Cargo.lock) and fails on test/lint failure. No placeholder success conditions are acceptable.

Rust core checks are executed via the Makefile:

- `make check` — Validate contract correctness, compile, format, and lint:
  - `contract-check` — Python contract validation
  - `cargo check --all --all-targets --locked` — Compilation with locked dependencies
  - `cargo fmt --all -- --check` — Code format validation (no in-place changes)
  - `cargo clippy --all-targets --locked -- -D warnings` — Lint pass with warnings-as-errors

- `make test` — Run contract and Rust test suites:
  - `contract-test` — Python contract tests
  - `cargo test --all --locked` — Full Rust test suite with locked dependencies

These checks are wired into `.github/workflows/core.yml` on Linux runners (ubuntu-latest) via GitHub Actions. The workflow runs on all pull requests to main and validates that the change compiles, passes tests, and meets lint standards with locked dependencies. 

Artifact retention is bounded: `core.yml` does not upload artifacts; any future CI artifact upload must set `retention-days ≤ 7` to limit storage and cost. Evidence collection is separate from CI: device/simulator results, live trial data, and provider integration tests are distinct artifacts and do not gate the Linux checks.

## Required checks for macOS workers

Native iOS checks run on macOS runners (macos-15) via `.github/workflows/ios.yml`. These checks validate the iOS project structure, simulator build, and unit tests for the exact submitted commit.

Native iOS checks executed via the workflow:

- **Project generation and simulator build** (`generate-build` job):
  - `scripts/generate.sh` — Generate `OhAnd.xcodeproj` using pinned XcodeGen via mint
  - `scripts/build-simulator.sh BridgeProbe` — Build one probe app for iOS Simulator without signing
  - Capture and log Xcode version (pinned to 16.4), Swift version, and Simulator SDK information

- **Unit tests and smoke test** (`test` job):
  - Dynamically selects first available iOS simulator from `xcrun simctl list -j devices available`
  - `xcodebuild test -scheme OhAndTests -sdk iphonesimulator` — Run native unit tests on selected simulator
  - Boot selected simulator and launch BridgeProbe app with deterministic boot status wait
  - Log selected simulator device name, runtime version, and UDID for reproducibility

Linux Odonian workers authorizing native checks for Swift/Objective-C code:

- When a PR touches native iOS code (files under `ios/` except generated/ignored paths), Linux workers must await completion of the macOS iOS CI jobs before review.
- Retrieve the workflow run URL via `gh run list --repo [repo] --commit [head-sha] --status completed --json url` to match the exact submitted commit, or link it directly in the PR/task. Do NOT use branch-based lookups, which may retrieve stale runs.
- Block review if the run is not available or if `generate-build` or `test` jobs fail; Linux-only validation cannot certify Swift/Objective-C.
- Confirm toolchain versions (Xcode, Swift, SDK), selected simulator device name/runtime/UDID, and test results are captured in the run logs; these establish that the build is reproducible for the exact revision reviewed.
- Retrieve and link the `ios-test-logs` artifact from the run (contains test output and xcresult bundle); this artifact is available for 7 days at the GitHub URL: `https://github.com/boldfield/ohand/actions/runs/{run-id}` → Artifacts.
- No signing credentials or private certificates are involved in these checks; fork PRs require no special permissions.

The first repository commit contains documentation and license material only. It is a bootstrap operation, not an application implementation or proof of completed milestones.
