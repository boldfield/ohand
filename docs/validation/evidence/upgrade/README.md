# Upgrade evidence

Records of installing a newer `OhAndApp` build over an older one.

* `simulator-<collection date>-<candidate build number>.json` is a copy of the `simulator-upgrade-record.json`
  that `tools/release/simulator_upgrade_check.sh` writes in CI (`.github/workflows/upgrade.yml`). Format
  `ohand.upgrade-simulator-record.v1`: the run, the prior and candidate revisions (full SHAs) and build numbers, the
  installed build number before seeding, after the upgrade and after a relaunch, whether the data container stayed the same,
  the simulator's name and runtime, the collection time, what was seeded (counts) and each check with its outcome.
* Device records (`ohand.upgrade-device-evidence.v1`) are written by the maintainer following
  `docs/testing/m1-upgrade-evidence.md`.

No record holds capture text, a team identifier, a profile, a certificate, a device identifier or any hash of one. The
simulator's UDID is left out too.
