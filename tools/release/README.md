# tools/release

Build identity, the signed trial candidate and the simulator upgrade check (T10a). See `docs/releases/m1-trial/README.md`
and `docs/testing/m1-upgrade-evidence.md`.

| File | Purpose |
| --- | --- |
| `build_info.py` | Derives version, build number (first-parent count), full revision and dirty flag from git; `--require-clean` for candidates |
| `sign_trial_candidate.sh` / `.py` | Wraps `tools/apple-build/sign_probe.py` to export the signed `OhAndApp`, hash it and write the candidate record |
| `simulator_upgrade_check.sh` | Builds a prior and the candidate revision, seeds the prior through its UI, upgrades in place and checks (macOS) |
| `verify_upgrade_report.py` | Judges the run from the app's content-free reports and writes the simulator record |
| `check_release_excludes_upgrade_report.sh` | Builds Release and fails if the debug-only report is in the binary (macOS) |

`make release-test` runs the Linux tests: derivation against throwaway git repositories, the signing wrapper against the stubbed
Apple tools of `tools/apple-build`, the verdict logic, and static checks of the wiring.
