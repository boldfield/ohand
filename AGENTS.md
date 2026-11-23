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
- Required CI and independent review remain gates even though human approval is not one. Use standard hosted runners and keep CI artifacts within the budget in `docs/features/ci-artifact-budget.md`; paid capacity requires separate authorization.
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

CI artifacts are budgeted, not only bounded in time. The `boldfield` account shares one GitHub Actions storage quota across every repository; ohand exhausted it on 2026-10-29 with 18 GB of seven-day evidence, and again on 2026-11-01 with 4.4 GB of failure bundles uploaded in thirty hours, which turned other repositories' required checks red both times (`docs/features/ci-artifact-budget.md`). The quota is shared with every other repository on the account, so none of the numbers below is a per-repository decision. `core.yml` uploads nothing. Every other upload step follows the budget: routine evidence (logs, screenshots, text and JSON records) uploads with `retention-days: 2`, a green `ios.yml` run stays under 25 MB of artifacts across all its jobs and a green `upgrade.yml` run under 5 MB; `.xcresult` and other result bundles are never uploaded, routine or not; a job that has failed uploads exported failure diagnostics instead (the bundle's test summary and the failed tests' attachments, written by `ios/scripts/export-failure-diagnostics.sh`) under the routine name with a `-failure` suffix and `retention-days: 1`, under 100 MB per run in total, so any run, green or failed, stays under 125 MB; every upload step sets `continue-on-error: true`, so a quota event degrades evidence instead of failing a required check. Acceptance criteria assert on files in the workspace, and reviewers read the logs inside the artifact. `ios/scripts/check_project_config.py` enforces the mechanical parts of this rule. Evidence collection is separate from CI: device/simulator results, live trial data, and provider integration tests are distinct artifacts and do not gate the Linux checks.

## Required checks for macOS workers

Native iOS checks run via `.github/workflows/ios.yml` on the standard, free GitHub-hosted `xcode-27` runner image (arm64, macOS 27.0.1) for every job, including those that boot a simulator (`simulator`, `notification-probe`, `capture-probe` and `walkthrough`), on every pull request to `main`, with Xcode pinned to 27.2 and its iOS 27 simulator SDK (matching `options.xcodeVersion` in `ios/project.yml`; `ios/scripts/check_project_config.py` fails if the workflow pin and `options.xcodeVersion` disagree, or if a job runs on a label other than `xcode-<pinned major>`). The simulator jobs stop the Xcode build service before the first boot, so the memory the build held is returned to the simulator; this arm64 layout was measured against `macos-26-intel` and kept (see the comments in `ios.yml`). The image carries the iOS 27.0 runtime with ordinary iPhone and iPad device types and the iOS 27.1 runtime with the iPhone Duo only, so `select_simulator.py` excludes the Duo from the `iPhone` family and the compact jobs run on an ordinary iPhone. The workflow has `contents: read` permission only, needs no secrets or signing material, and therefore also runs for fork PRs. A simulator test step or app-launch smoke script that failed is retried once, after a warm re-preparation of the device, only when every failure in its log matches the closed list of launch and device signatures in `ios/scripts/classify_simulator_fault.py` (so no assertion was judged; one failed assertion of any other kind means no retry), through `ios/scripts/retry-simulator-step.sh`, which keeps the first log as `<log>.attempt-1` and records the retry in `simulator-recoveries.txt` with kind `test-launch`. The simulator work is three parallel jobs (CI07), each on its own runner with its own single cold boot; each compiles only what it runs before that boot, and each records `toolchain.txt`, `simulator.txt`, `host-pressure.txt` and `simulator-recoveries.txt` in its own artifact. `ios/scripts/check_project_config.py` fails if a step of the former single job is missing from the three or runs in two of them. The workflow does not start for a pull request whose every changed file is under `docs/` or is a Markdown file at the repository root (`*.md`, including `AGENTS.md`), through the trigger's `paths-ignore` (CI08; the list is closed, and a change under `core/`, `ios/`, `app/`, `tools/`, `.github/` or anywhere else starts it), so a documents-only task needs no macOS run. Every job:
- records `sw_vers`, Xcode, Swift, simulator SDK and the available simulators (`toolchain.txt`);
- picks one available iPhone simulator with `ios/scripts/select_simulator.py` (newest iOS runtime not newer than the pinned SDK) and logs its name, runtime and UDID (`simulator.txt`);
- generates the project with pinned XcodeGen (installed by `ios/scripts/install-xcodegen.sh` from the release archive, checked against a SHA-256 recorded in the script and cached by version and digest, then `ios/scripts/generate.sh`), compiles its own targets first so the compiler and a booted simulator never run together, stops the build service, and cold-boots the selected device once through `prepare-simulator.sh --erase` (the preparation fails, printing a host snapshot, if the simulator does not answer 30 probes in a row within 3 s each inside 480 s; a failed gate gets exactly one recovery, which shuts down every simulator, restarts the CoreSimulator service and repeats the same preparation with the same gates, and each recovery is recorded in `simulator-recoveries.txt`);
- keeps each test exit status (`pipefail`), so a failure to find a device or run tests fails the job.

The jobs differ in what they run:

- `simulator` builds and runs OhAndTests (`OhAndTests.log`), the credential relaunch check, the CredentialProbe Keychain self-test, and the launch smokes for BridgeProbe (`smoke.log`, which ends `Smoke test passed`) and CredentialProbe (`credential-smoke.log`); it also builds AudioProbe, which is not run. Artifact `ios-evidence` (logs, screenshots, `host-pressure.txt`; no `.xcresult` bundle), retention 2 days.
- `notification-probe` builds the NotificationProbe UI tests, cold-boots the device once, shuts it down and clones the settled, empty device into a template (the source device is not booted again), and runs each NotificationProbe suite on its own clone of that template (booted, waited until responsive, then deleted), so no suite pays for a first boot. Artifact `ios-evidence-notification`: the suite logs under `notification-probe/`, `prepare-settle.log`, `host-pressure.txt`. The three jobs that replaced the single simulator job share its former 24 MB failure-diagnostics allowance (`CAP_BYTES` 8 MB each), and `check_project_config.py` fails if their caps sum to more.
- `capture-probe` builds the CaptureProbe UI tests, CaptureProbe, OhAndApp and TranscriptionProbe, runs the handoff UI test with its ingress smoke (`capture-smoke.log`, `capture-uitests.log`), then the OhAndApp launch smoke (`ohandapp-smoke.log`), the OhAndApp embedded-framework check (`OhAndApp-bundle.txt`) and the TranscriptionProbe launch smoke (`transcription-smoke.log`). Artifact `ios-evidence-capture`, retention 2 days. Each smoke script shuts the device down when it ends, so every smoke after a job's first pays a warm boot; the launch smokes are divided between `simulator` and `capture-probe` to keep both jobs under 50 minutes.
- A job that failed uploads exported failure diagnostics (no `.xcresult` bundle) as `<its artifact>-failure` (`ios-evidence-failure`, `ios-evidence-notification-failure`, `ios-evidence-capture-failure`), `retention-days: 1`.

The `walkthrough` job is a matrix of two jobs, compact (iPhone) and regular (iPad), each with one cold boot; each uploads `ios-walkthrough-<width>` (retention 2 days, result bundles only as `ios-walkthrough-<width>-failure` after a failure) including `host-pressure.txt` (diagnostic only, not a gate): label, kind (cold-boot, warm-boot or clone), time to responsive, highest 1-minute load and lowest free memory per preparation.

The `accessibility` job is a matrix of two jobs, `accessibility (group a)` and `accessibility (group b)` (75-minute limit each). Each runs `ios/scripts/accessibility-simulator.sh` for its half of the tests, listed in `ios/scripts/accessibility-groups.txt`, at the default and then the largest text size on its own simulator, and uploads `ios-accessibility-<group>` (routine evidence, `retention-days: 2`) and, on failure, `ios-accessibility-<group>-failure` (`retention-days: 1`). The two artifacts together stay inside the budget above: a green run's accessibility artifacts total under 10 MB, and the failure exports stay under 100 MB per run in total. See `docs/validation/accessibility.md`.

Linux checks cover the native tooling that runs on any host: `make check` runs `ios-check` (`ios/scripts/check_project_config.py` plus the unit tests for it and for `select_simulator.py`). They do not prove that Swift compiles.

Linux Odonian workers whose change touches native code (anything under `ios/`, or `.github/workflows/ios.yml`) must, for the exact submitted commit:

1. Push the branch and open or update the PR. Take the commit with `head_sha=$(git rev-parse HEAD)`.
2. Find the run for that commit, not for the branch: `gh run list --workflow ios.yml --commit "$head_sha" --json databaseId,headSha,status,conclusion,url`. Require `headSha` to equal `$head_sha`; if there is no run yet, wait and poll (the workflow starts on PR push; `gh workflow run` is unnecessary because it only triggers on `pull_request`).
3. Wait for completion with `gh run watch <databaseId> --exit-status --interval 60`. Always pass the interval: the default polls every 3 seconds, and a fleet of watchers on 20-minute macOS runs exhausts the shared GitHub API quota (5000 requests per hour per user), which then fails every merge with a 403. The same applies to `gh pr checks --watch` (use `--interval 60`) and to any hand-written polling loop (sleep at least 60 seconds between calls). The run must conclude `success`; a failed, cancelled or still-queued run means the work is not done. Do not rerun until it passes by lowering checks.
4. Link the run URL in the PR description and the task result. Retrieve the evidence with `gh run download <databaseId> --name ios-evidence` (kept 2 days; rerun the workflow if it has expired) and confirm that `toolchain.txt` and `simulator.txt` name the Xcode, SDK, simulator, runtime and UDID, that `OhAndTests.log` contains `** TEST EXECUTE SUCCEEDED **` (what `xcodebuild test-without-building` prints; `** TEST SUCCEEDED **` is only printed by a build-and-test run), and that `smoke.log` ends with `Smoke test passed`.
5. If macOS runners are unavailable (no run starts, runs stay queued, or the service is down), block the task with `odonian transition <id> --to blocked --note "<reason>"`. Linux-only validation cannot certify Swift.

**Tasks the board waits on (`odonian show <id>` prints `Await Checks: true`).** Since 2026-11-22 the Odonian checks gate waits for CI, so a worker on such a task does not: hosted macOS runs queue for hours, and a worker that waits holds a fleet slot other projects need. For these tasks steps 2 to 5 change as follows, and nothing else in this document does.

- Push, open or update the PR (step 1), run the Linux checks (`make check`, `make test`), and submit at once. Do not run `gh run watch`, do not poll, and do not block the task because a run is queued.
- The board moves the task to review only when every check on the head commit is green, and returns it with the failed checks as PR feedback when one is red. On that rework, read the failed job's log before changing anything: fix a failure the branch caused; for a runner or network fault (for example a failed download or clone in a setup step), rerun the failed jobs with `gh run rerun <databaseId> --failed`, say so in the acknowledgement, and submit again without waiting.
- An acceptance criterion that asks for the exact-head `ios.yml` run is met by the run the gate passed. The reviewer finds it with the step 2 command for the PR's head commit, confirms `headSha` and `success`, and reads the evidence artifact as step 4 describes. The worker does not link a run it has not seen pass.
- A criterion that needs more than one attempt of a job on the same head (for example three consecutive passes of a flaky step) cannot be met without waiting; such a task is created without `await_checks`, and the worker follows steps 2 to 5 as written.

The first repository commit contains documentation and license material only. It is a bootstrap operation, not an application implementation or proof of completed milestones.
