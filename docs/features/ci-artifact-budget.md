# CI artifact budget

Status: active since 2026-10-31, revised 2026-11-01. Owner: coordinator. Tasks: CI01 (landed 2026-11-01, PR #391), CI02, CI03 (sections below); U10a amended in `m1-task-refinement.md` and paused until CI02 and CI03 land.

## Problem

The `boldfield` GitHub account shares one Actions artifact storage quota across every repository. On 2026-10-29 the quota was hit, and from then on every `actions/upload-artifact` step on the account failed with "Artifact storage quota has been hit". Other repositories whose required checks upload a report with `if: always()` went red on every branch even when their tests passed, and their Odonian boards stalled on approvable code.

ohand was the source. On 2026-10-31 it held 539 live artifacts, 17.8 GB, all created in the previous seven days, so the existing `retention-days: 7` rule was honored and insufficient. `ios.yml` runs on every pull request push, 13 to 41 runs a day that week, and each run uploaded four evidence bundles:

| artifact | per run | of which result bundles |
|---|---|---|
| ios-evidence | 121 MB | 115 MB (`OhAndTests.xcresult`; probe suites add about 1.5 MB) |
| ios-walkthrough-compact | 16 MB | 10 MB (two appearance bundles) |
| ios-walkthrough-regular | 11 MB | about 6 MB |
| tauri-probe-evidence | 2.6 MB | none |
| upgrade-evidence (upgrade.yml) | 68 MB | 67 MB (`seed.xcresult`) |

Measured on run 41100102984 and the 2026-10-27 upgrade runs. The logs, screenshots and text records that AGENTS.md's review procedure reads are about 5 MB in ios-evidence, 5 to 6 MB per walkthrough width and under 1 MB in upgrade-evidence. The U10a branch (PR #390) added a fifth bundle, `ios-accessibility`, at 458 MB per run; at the week's run rate that alone refills the quota in a day.

Immediate action taken on 2026-10-31: the maintainer deleted the live ohand artifacts except those of the two open pull-request heads, which in-flight exact-head reviews still download. Evidence is reproducible by rerunning the workflow.

### Second measurement, 2026-11-01

Thirty hours after the purge the quota was exhausted again. Live ohand artifacts at 2026-11-01T03:02Z:

| artifact | count | total |
|---|---|---|
| ios-accessibility-failure | 5 | 2268 MB (about 450 MB each) |
| ios-evidence | 14 | 1018 MB |
| ios-accessibility | 6 | 489 MB |
| ios-walkthrough-compact | 15 | 447 MB |
| ios-walkthrough-regular | 14 | 139 MB |
| ios-evidence-failure | 1 | 87 MB |
| tauri-probe-evidence | 15 | 16 MB |
| upgrade-evidence | 3 | 1 MB |
| total | 73 | 4465 MB |

Rule item 4 as first written (bundles upload only when the job has failed) did not bound anything: a branch whose job fails on every push uploads the bundle on every push. U10a's audit job on PR #390 failed on six consecutive pushes while its fixes were in progress, so its 448 MB bundle became the single largest item on the account. The `ios-evidence` rows are pushes from branches not yet rebased over CI02. Retention is irrelevant at this volume: the cap is hit within a day, before anything expires.

Actions on 2026-11-01: U10a was paused (blocked, then made to depend on CI02 and CI03) and its run was left to the maintainer to cancel; the maintainer deleted every live ohand artifact except those of the CI02 pull-request head, whose reviewers were downloading exact-head evidence; rule item 4 was rewritten (below) and CI03 added. CI02's green run on its PR branch (run 41259108144) uploaded `ios-evidence` 1 MB, `ios-walkthrough-compact` 4 MB, `ios-walkthrough-regular` 3 MB and `tauri-probe-evidence` under 1 MB: 8 MB for the run.

## The rule

Recorded in AGENTS.md. Each upload step in every workflow:

1. uploads routine evidence only: logs, screenshots, text and JSON records. Result bundles (`.xcresult`) and other build products are never in a routine artifact;
2. declares `retention-days: 2`. Reviews download exact-head evidence within an hour of submission; a review that is delayed longer reruns the workflow;
3. keeps a green `ios.yml` run under 25 MB of artifacts across all its jobs, and a green `upgrade.yml` run under 5 MB. Screenshots are the bulk of the remainder and are the walkthrough's deliverable, which is why a 10 MB target was not adopted;
4. never uploads a result bundle, even when the job has failed. A failed job uploads exported failure diagnostics instead, under the job's routine name with a `-failure` suffix and `retention-days: 1`: the bundle's test summary and test list as JSON, and the attachments (screenshots) of the failed tests only, written by `ios/scripts/export-failure-diagnostics.sh`. A run's failure artifacts total under 100 MB across all its jobs; the script stops adding attachments at the cap and records what it left out, so any run, green or failed, uploads under 125 MB. A full bundle is reproduced by rerunning the suite, locally or in CI. (Revised 2026-11-01; the first version uploaded whole bundles on failure and is the reason for the second measurement above);
5. sets `continue-on-error: true`, so a quota event degrades evidence instead of failing a required check. Acceptance criteria assert on files in the workspace; reviewers read the logs inside the artifact.

`ios/scripts/check_project_config.py` (run by `make check` through `ios-check`) enforces the mechanical parts of the rule so a later workflow edit cannot silently regress it.

Non-goals: changing where the scripts write their result bundles, changing which tests run, or reducing run frequency. Those stay as they are.

## CI01

Make every CI artifact upload non-fatal to its job

The first and smallest change, landed alone so it protects other repositories as early as possible. Every `upload-artifact` step in `.github/workflows/ios.yml` and `.github/workflows/upgrade.yml` gains `continue-on-error: true` and nothing else changes in those steps. `check_project_config.py` fails when any upload step in either workflow lacks it, with unit tests for the passing and failing cases.

Owned paths: the two workflow files (upload steps only), `ios/scripts/check_project_config.py` and its unit tests.

Dependencies: none. CI02 edits the same steps and depends on CI01.

Acceptance: the four upload steps set `continue-on-error: true`; the checker enforces it with tests; the exact-head macOS run for the submitted commit is green and linked in the PR body and the task result; AGENTS.md is untouched.

## CI02

Keep result bundles out of routine CI artifacts and cut retention to 2 days

Applies rule items 1 to 4 to both workflows. Routine artifacts exclude `.xcresult` bundles (exclusion patterns on the upload step, or moving the bundles aside before upload) and declare `retention-days: 2`. Each job that writes result bundles (`simulator`, both `walkthrough` widths, and the upgrade job) gains one failure-only upload step for the bundles under the `-failure` name, with `retention-days: 2` and `continue-on-error: true`. The checker fails on a routine upload step whose retention exceeds 2 days or whose path can include a bundle without an exclusion. AGENTS.md's description of the simulator job's upload and the 7-day evidence window is updated to match; the rule paragraph itself is unchanged.

Owned paths: the two workflow files (upload steps plus the new failure-only steps), `ios/scripts/check_project_config.py` and its unit tests, and the two descriptive sentences in AGENTS.md's "Required checks for macOS workers" section.

Dependencies: CI01.

Acceptance: no routine artifact holds a bundle; retention is 2 days everywhere; a green `ios.yml` run uploads under 25 MB in total and a green `upgrade.yml` run under 5 MB, listed per artifact from the Actions API in the task result; the failure path is shown once on the PR branch with a deliberately failing test commit that is then removed, with both runs linked; the checker enforces items 1 and 2 with tests; AGENTS.md matches; exact-head macOS evidence as AGENTS.md requires.

Revised 2026-11-01: CI02 lands as specified, with its failure-only steps uploading whole bundles at `retention-days: 2`, because it was already under review when rule item 4 was rewritten. CI03 replaces those steps. CI02 is not reopened for this.

Ownership amendment (coordinator, 2026-11-01, after the third round blocked): `tools/release/test_upgrade_check_structure.py` has a test, `test_artifacts_are_kept_for_seven_days`, that pins every `upgrade.yml` upload at `retention-days: 7`, so acceptance item 2 cannot pass `make test` without editing it. CI02 owns that one test: rename it and assert the budget's 2 days for every upload step in `upgrade.yml`. Nothing else in that file changes.

## CI03

Cap failure diagnostics: export from the bundle, under 100 MB, retention 1 day

Applies revised rule item 4. The failure-only upload steps that CI02 added to `simulator`, both `walkthrough` widths and the upgrade job stop uploading `.xcresult` bundles. Each of those jobs gains, before its failure-only upload, one step that runs only when the job has failed (`if: failure()`, `continue-on-error: true`) and calls a new script, `ios/scripts/export-failure-diagnostics.sh`, with the job's bundles and an output directory under `ios/.evidence/`. The failure-only upload step uploads that directory (not the bundles), keeps the `-failure` name, and declares `retention-days: 1`, `continue-on-error: true` and `if-no-files-found: ignore`.

The script, per bundle: writes the test summary and the test list as JSON with `xcrun xcresulttool get test-results summary` and `... tests` (verify the exact subcommands and flags against the pinned Xcode in `ios/project.yml`; the legacy interface is not required); derives the failed tests from the test list; exports the attachments of those tests only (`xcrun xcresulttool export attachments` with its per-test selector, or export all and keep only files the export's own manifest attributes to a failed test); adds attachments in test order until the output directory reaches a 100 MB cap shared across the bundles it was given, then stops; and writes `MANIFEST.txt` listing every file kept and every attachment omitted, with sizes. It exits 0 when the bundle is missing or unreadable, writing that fact to the manifest, because the job has already failed and a diagnostics problem must not change its conclusion.

`check_project_config.py` fails when any upload step in either workflow, routine or failure, has a path that can include a `.xcresult` bundle without an exclusion, when a failure-only upload step declares `retention-days` other than 1, or when a failure-only upload step is not preceded by the export step. Tests for the passing and failing cases.

Owned paths: the two workflow files (the failure-only upload steps and the new export steps; the routine steps are unchanged), `ios/scripts/export-failure-diagnostics.sh` (new), `ios/scripts/check_project_config.py` and its unit tests, and the retention test in `tools/release/test_upgrade_check_structure.py` that CI02 set to 2 days for every `upgrade.yml` upload: CI03 makes it assert 2 days on routine steps and 1 day on `-failure` steps, and nothing else in that file (added 2026-11-01). AGENTS.md is already updated and is not touched.

Dependencies: CI02 (same steps; CI03 rebases over it).

Ownership amendment (coordinator, 2026-11-02, after the second round blocked): `ios/scripts/test_prepare_simulator.py` has a test, `test_workflow_does_not_weaken_tests`, that allows `continue-on-error` only on `actions/upload-artifact` steps, so the export step this task requires cannot pass `make test` without it. CI03 owns that one assertion: it also allows `continue-on-error` on a step that runs `ios/scripts/export-failure-diagnostics.sh` and is conditioned on `failure()`, and on nothing else. A test or build step must still never be non-fatal. Nothing else in that file changes. (CI02 landed on 2026-11-01 as PR #394 through its Opus escalation.)

Acceptance: no upload step in either workflow can upload a bundle; every failure-only step declares `retention-days: 1` and uploads the export directory; the export is shown once on the PR branch with a deliberately failing test commit that is then removed, with both runs linked and the failure artifact's name and size from the Actions API quoted in the PR body, under 100 MB; the manifest in that artifact names the failed test and its attachments; a green `ios.yml` run on the final head still uploads under 25 MB in total, listed per artifact; the checker enforces the three rules with tests; exact-head macOS evidence as AGENTS.md requires.

## U10a

The `ios-accessibility` artifact must follow the same rule before U10a lands: audit log, summary and large-text screenshots only, `retention-days: 2`, `continue-on-error: true`, no bundle upload, and the whole `ios.yml` run under 25 MB when green. Its failure-only step follows CI03: the export script on `ios/.evidence/accessibility-*.xcresult`, uploaded as `ios-accessibility-failure` with `retention-days: 1`. Recorded as amendments in the U10a section of `m1-task-refinement.md`; U10a was paused on 2026-11-01 and depends on CI02 and CI03, and rebases over both.

## Verification after the fix

The Actions artifacts API for the repository should show routine artifacts of a few megabytes each and a total that stays in the low hundreds of megabytes at the week's run rate. A rerun of another repository's upload step should pass once GitHub has recalculated usage (6 to 12 hours after the deletion).
