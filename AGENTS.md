# Oh And agent instructions

Read [DESIGN.md](DESIGN.md) before planning or implementing work.

For M1 implementation, also read [the task refinement overlay](docs/features/m1-task-refinement.md) and its [effective dependency graph](docs/features/m1-task-refinement.json). It delegates original ownership groups into smaller tasks, preserves their acceptance criteria, and corrects transcription/reset ownership. The original task manifest remains the historical baseline; this overlay defines current execution.

## Interactive coordination

Interactive agents investigate, discuss, and maintain design/specification documents. Application code changes go through Odonian tasks. Direct documentation edits and repository bootstrap mechanics are allowed. Do not implement features or fixes directly unless the maintainer explicitly requests a direct edit.

## Project-specific autonomous delivery

This project explicitly authorizes agent-driven delivery with **no routine human approval or merge gate**. This overrides generic Odonian skill defaults calling for human approval of each design choice, task, milestone, merge, or release. It does not remove independent review, required checks, or real test evidence, and does not change other projects' policies.

- Decompose build/design work into small, single-purpose tasks. Set initial `model=sonnet` for code changes (Rust core, Swift, FFI, tools and test harnesses) and `model=haiku` only for docs, configuration and evidence-recording tasks; set `escalate=true` on both. Rationale recorded 2026-10-08: across 39 landed tasks on this board, Haiku landed 2, both scaffolding, and every traced chain escalated after four rejected rounds; the overlay's Haiku-sized slices escalated at the same rate as unsliced tasks, so the failure mode is reviewer rigor at the Rust/Swift boundary, not task size.
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

For P08/V08 external execution inputs, read [the prerequisite status and provider configuration reference](docs/features/m1-external-prerequisites.md). Neither task is complete until its original real-evidence criteria pass.

For optional paired-model comparison tasks, read [the comparison specification](docs/features/model-comparison.md) and [task ownership/dependency overlay](docs/features/model-comparison-tasks.json). These authorize only listed additive/shared-file changes, preserve baseline acceptance, and keep benchmark work off the base M1 dependency graph.

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

Native iOS checks run on the standard GitHub-hosted `macos-26` runner via `.github/workflows/ios.yml` on every pull request to `main`, with Xcode pinned to 26.6 and its iOS 26 simulator SDK (matching `options.xcodeVersion` in `ios/project.yml`; `ios/scripts/check_project_config.py` fails if the workflow pin and `options.xcodeVersion` disagree). The `tauri-probe` job uses the same runner and pin. The next bump is to Xcode 27 once hosted images carry it. The workflow has `contents: read` permission only, needs no secrets or signing material, and therefore also runs for fork PRs. The `simulator` job:

- records `sw_vers`, Xcode, Swift, simulator SDK and the available simulators (`ios-evidence/toolchain.txt`);
- picks one available iPhone simulator with `ios/scripts/select_simulator.py` (newest iOS runtime not newer than the pinned SDK) and logs its name, runtime and UDID (`ios-evidence/simulator.txt`);
- generates the project with pinned XcodeGen (`ios/scripts/generate.sh`) and runs `xcodebuild test -scheme OhAndTests` on that UDID, keeping the exit status (`pipefail`), so a failure to find a device or run tests fails the job;
- builds `BridgeProbe` unsigned (`ios/scripts/build-simulator.sh BridgeProbe`);
- runs the launch smoke test `ios/scripts/smoke-simulator.sh`: boot with `simctl bootstatus -b`, install `BridgeProbe.app`, launch `com.boldfield.ohand.probes.bridge`, verify it is still running, capture a screenshot;
- uploads `ios-evidence` (logs, `OhAndTests.xcresult`, screenshot) with `retention-days: 7`.

Linux checks cover the native tooling that runs on any host: `make check` runs `ios-check` (`ios/scripts/check_project_config.py` plus the unit tests for it and for `select_simulator.py`). They do not prove that Swift compiles.

Linux Odonian workers whose change touches native code (anything under `ios/`, or `.github/workflows/ios.yml`) must, for the exact submitted commit:

1. Push the branch and open or update the PR. Take the commit with `head_sha=$(git rev-parse HEAD)`.
2. Find the run for that commit, not for the branch: `gh run list --workflow ios.yml --commit "$head_sha" --json databaseId,headSha,status,conclusion,url`. Require `headSha` to equal `$head_sha`; if there is no run yet, wait and poll (the workflow starts on PR push; `gh workflow run` is unnecessary because it only triggers on `pull_request`).
3. Wait for completion with `gh run watch <databaseId> --exit-status --interval 60`. Always pass the interval: the default polls every 3 seconds, and a fleet of watchers on 20-minute macOS runs exhausts the shared GitHub API quota (5000 requests per hour per user), which then fails every merge with a 403. The same applies to `gh pr checks --watch` (use `--interval 60`) and to any hand-written polling loop (sleep at least 60 seconds between calls). The run must conclude `success`; a failed, cancelled or still-queued run means the work is not done. Do not rerun until it passes by lowering checks.
4. Link the run URL in the PR description and the task result. Retrieve the evidence with `gh run download <databaseId> --name ios-evidence` (kept 7 days) and confirm that `toolchain.txt` and `simulator.txt` name the Xcode, SDK, simulator, runtime and UDID, that `OhAndTests.log` contains `** TEST SUCCEEDED **`, and that `smoke.log` ends with `Smoke test passed`.
5. If macOS runners are unavailable (no run starts, runs stay queued, or the service is down), block the task with `odonian transition <id> --to blocked --note "<reason>"`. Linux-only validation cannot certify Swift.

The first repository commit contains documentation and license material only. It is a bootstrap operation, not an application implementation or proof of completed milestones.
