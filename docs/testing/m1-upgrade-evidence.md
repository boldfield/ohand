# Upgrading a trial build keeps the person's data

Installing a newer build of Oh And over an older one must keep what the person saved: sources, corrections, deletions, the
provider profile with its credential, and scheduled reminders. This document says how that is shown on a simulator (automatic,
fleet-owned) and how the maintainer shows it on the trial iPhone (manual, maintainer-owned). Neither records capture text.

## Build identity

Every build names itself in its `Info.plist` (version, build number, full git revision, dirty flag; see
`docs/releases/m1-trial/README.md`), and a metrics export (`ohand.metrics.v1`) carries the same four values in a `build` object.
The field is additive: a reader that ignores unknown fields is unaffected, so the schema label did not change. The app supplies
the values (`CoreMetricsBuild.current()`); the core accepts only a short version label, a numeric build number, a 40-digit
lowercase hexadecimal revision and a flag, and refuses anything else, so no text can enter the report that way.

## Simulator upgrade check (automatic)

`tools/release/simulator_upgrade_check.sh` runs in `.github/workflows/upgrade.yml` for pull requests that touch
`tools/release/`, `ios/UITests/Upgrade/`, `ios/AppAssembly/UpgradeCheck/` or the core store's schema, and on demand. It

1. builds `OhAndApp` (Debug, simulator) at a prior revision (a git worktree of an earlier `main` commit; for a pull request the
   merge base) and at the candidate revision;
2. installs the prior build and reads its build number with `simctl appinfo`;
3. seeds it by driving its UI (`ios/UITests/Upgrade/UpgradeSeedUITests.swift`, a UI-test bundle with no target application, so
   xcodebuild never installs or replaces the app): four typed captures, one correction of a capture's words, one deletion, one
   reminder scheduled on an item changed to an action, and a cloud provider profile with a stored secret that is made the
   default and allowed for processing. Every write goes through the app's own services. Nothing writes to a database, a Keychain
   or a file of the app;
4. installs the candidate over it with `simctl install` (the script never uninstalls or erases the app; its only erase is the
   first cold boot, before anything is installed) and records the build number and the data container again;
5. launches the candidate with `-OhAndUpgradeReport`. The debug-only report writer waits for the app's own Face ID
   authentication (the script presents a matching face, as the screenshot walkthrough does) and then writes
   `ohand.upgrade-report.v1` to the app's data container;
6. checks the upgraded app through its UI (`UpgradeVerifyUITests`: the captures and the correction are shown, the deleted capture
   is in neither the recents nor a search, the reminder shows the time it was set for), relaunches it for a second report, and
   judges everything with `tools/release/verify_upgrade_report.py`.

The report holds counts, states, identifiers that the synthetic run created and instants, plus the length of each text. It never
holds text. It is compiled out of Release builds: every file under `ios/AppAssembly/UpgradeCheck/` is wrapped in `#if DEBUG`
(checked on Linux by `tools/release/test_upgrade_check_structure.py`), and the `release-excludes-report` job builds a Release app
and fails if the binary contains any of the report's strings.

What the check requires (each is a named check in the record):

* the installed build number rose from the prior build's to the candidate's, and the same candidate number holds after the
  relaunch; the data container is the same after the upgrade and after the relaunch, and its contents carried over the upgrade:
  the script writes a marker file into the prior container before the upgrade and finds it in the container afterwards. The
  record also says whether the container's path changed. On the simulator `simctl install` over an installed app can give the
  data a new container directory (the identifier in the path) while keeping its contents, so the path is recorded, not required
  to be equal. iOS does not promise a stable absolute container path across an install, so the device procedure relies on the
  contents carrying over, not on the path;
* every seeded source is present, active and has the length it was typed with; the corrected capture's authoritative text is the
  correction and its original is kept;
* the deleted item is not found and not in the recents;
* the provider profile is bound to a credential that is still present, is the default for text interpretation, and a route grant
  covers that exact version, so processing still routes to it;
* the reminder has the same identifier and fire time after the relaunch, and exactly one pending notification request exists for
  it (`<reminder id>#<generation>`), with no other reminder request;
* when the prior revision also contains the report, its reminder identifier, fire time and pending requests equal the
  candidate's.

Limits, stated plainly. The prior revision for this task's own evidence is the pull request's merge base, which does not yet
contain the report, so the pre-upgrade reminder identifier is not read from it; the fire time before the upgrade is compared
through the label the reminder section shows, and the identifier is compared between the first launch after the upgrade and the
relaunch. The Add provider screen of this build offers only fixed cloud providers (the private-server option is switched off),
so the profile's endpoint is the provider type's own; the secret, name and model are synthetic and nothing is sent: the profile
is made the default after the captures were saved, and no capture is made afterwards.

The first committed result is `docs/validation/evidence/upgrade/simulator-2026-10-27-362.json`: CI run 40544423613 on
`7ae9568` (candidate build 362) upgrading from the pull request's merge base `6d26d65` (build 1: it predates the build identity, so it
carries the source plist's number). All checks passed. The committed record came from that run's head; the head of the pull request
also contains the record and this paragraph, and its own exact-head run is linked in the pull request.

The simulator's result is committed under `docs/validation/evidence/upgrade/` (format in the README there).

## Procedure for the trial phone (maintainer; section for T10b)

Do not paste capture text into any record; write counts and outcomes only.

1. **Produce the candidate.** On a clean, full-history checkout of `main` run
   `tools/release/sign_trial_candidate.sh --output-dir <directory outside the repository>` with the signing inputs described in
   `docs/releases/m1-trial/README.md`. Commit the candidate record it writes. Note the build number.
2. **Before upgrading,** on the phone's current install, unlock the app and note, as counts: saved captures, how many have a
   correction, how many were deleted, whether Settings → Processing shows the provider you use, and for one scheduled reminder the
   time its item shows. Do not delete or reinstall the app.
3. **Upgrade in place.** Install the candidate's `.ipa` over the existing app (for example
   `xcrun devicectl device install app --device <device> <path to the extracted app>`), never uninstalling it first. Open the app.
4. **Check through the app.** After unlocking: the captures are all listed with the same counts; a corrected capture shows its
   correction and Source detail still offers the original; the deleted captures are not in the recents and a search for words
   from one finds nothing; Settings → Processing shows the same provider as before and Check saved secret reports it present; the
   scheduled reminder shows the same time, and a notification still arrives for it (or is still pending in Settings → Status).
5. **Record the result** as a file `docs/validation/evidence/upgrade/device-<collection date>-<candidate build number>.json` in
   this format (`ohand.upgrade-device-evidence.v1`):

```json
{
  "schema": "ohand.upgrade-device-evidence.v1",
  "collected_at": "<UTC time>",
  "candidate_record": "docs/releases/m1-trial/candidate-<build>-<revision prefix>.json",
  "installed_build_number_before": "<number>",
  "installed_build_number_after": "<number>",
  "counts_before": {"captures": 0, "corrections": 0, "deleted": 0, "reminders": 0},
  "counts_after": {"captures": 0, "corrections": 0, "deleted": 0, "reminders": 0},
  "provider_profile_still_configured": true,
  "credential_still_present": true,
  "reminder_time_unchanged": true,
  "reminder_notification_seen": true,
  "outcome": "passed",
  "notes": "<counts and outcomes only; no capture text>"
}
```

   A device record holds no team identifier, profile, certificate, device identifier or hash of one. If any check fails, record
   `"outcome": "failed"` with the check named in `notes`, and do not delete the app: keep it for diagnosis.
