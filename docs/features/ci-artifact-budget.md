# CI artifact budget

Status: active since 2026-10-31. Owner: coordinator. Tasks: CI01, CI02 (sections below); U10a amended in `m1-task-refinement.md`.

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

## The rule

Recorded in AGENTS.md. Each upload step in every workflow:

1. uploads routine evidence only: logs, screenshots, text and JSON records. Result bundles (`.xcresult`) and other build products are never in a routine artifact;
2. declares `retention-days: 2`. Reviews download exact-head evidence within an hour of submission; a review that is delayed longer reruns the workflow;
3. keeps a green `ios.yml` run under 25 MB of artifacts across all its jobs, and a green `upgrade.yml` run under 5 MB. Screenshots are the bulk of the remainder and are the walkthrough's deliverable, which is why a 10 MB target was not adopted;
4. uploads result bundles only when the job has failed, under the job's routine name with a `-failure` suffix, with `retention-days: 2`;
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

## U10a

The `ios-accessibility` artifact must follow the same rule before U10a lands: audit log, summary and large-text screenshots only, `retention-days: 2`, `continue-on-error: true`, bundles only on failure, and the whole `ios.yml` run under 25 MB. Recorded as an amendment in the U10a section of `m1-task-refinement.md`; U10a rebases over CI01 and CI02.

## Verification after the fix

The Actions artifacts API for the repository should show routine artifacts of a few megabytes each and a total that stays in the low hundreds of megabytes at the week's run rate. A rerun of another repository's upload step should pass once GitHub has recalculated usage (6 to 12 hours after the deletion).
