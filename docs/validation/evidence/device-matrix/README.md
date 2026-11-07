# Device-matrix records (`ohand.device-matrix.v1`)

This directory holds the sanitized records of the maintainer's run of the M1 device-matrix procedure
([`docs/validation/m1-device-results.md`](../../m1-device-results.md)) on the trial iPhone. T05a defines the format and
its checker; T05b commits the record. Until a record is committed the directory holds only this file, and
`make device-matrix-check` passes.

A record is named `device-<collection date>-<candidate build number>.json`, for example `device-2026-11-12-412.json`.
It is modelled on `docs/validation/evidence/device-feasibility/2026-10-09-device-matrix.json`, but is machine-checked:
`tools/validation/device_matrix/check_device_matrix.py RECORD` exits non-zero and names each problem, and
`make device-matrix-check` runs it over every `*.json` file here.

Raw recordings, screenshots, Status captures and anything showing the home or lock screen stay with the maintainer.
The record names artifacts and refers to the private evidence; it does not contain them.

## Fields

Every key is required unless marked optional. A key not listed here is refused.

| Key | Value |
| --- | --- |
| `schema` | `"ohand.device-matrix.v1"` |
| `collected_at` | ISO 8601 timestamp with offset (the end of the last session) |
| `candidate_record` | path of the candidate record, directly under `docs/releases/m1-trial/`; the file must exist |
| `build_number` | string of digits; equals `build_number` in the candidate record |
| `revision` | 40-digit lowercase git SHA; equals `revision` in the candidate record |
| `device` | `{ "model": "iPhone 16 Pro", "ios_version": "26.6.2" }`; these two keys only |
| `capabilities_enabled` | list drawn from `self_hosted_provider`, `shadow`, `previews`; empty when none (see "Capabilities" in the procedure) |
| `cases` | object keyed by case identifier, one entry for every identifier in `tools/validation/device_matrix/required_cases.json`, and no other key |
| `latency` | object keyed by series name, see below |
| `private_evidence_reference` | text saying where the private evidence is kept and how to audit it; no path to a person's account |
| `issues` | list, empty when the run found none |

### `cases.<id>`

| Key | Value |
| --- | --- |
| `outcome` | `passed`, `failed`, `blocked` or `not_applicable` |
| `artifacts` | list of artifact names, each `<id>-<detail>.<extension>`; required (at least one) for `passed` and `failed` |
| `observed_at` | ISO 8601 timestamp with offset; required for `passed` and `failed` |
| `reason` | required for `blocked` and `not_applicable`; for `not_applicable` exactly `capability not enabled in candidate` (a case whose capability is not in `capabilities_enabled`) or `Mac inlet not included in candidate` (`state-6`, `plan-mac-asleep`); optional otherwise |
| `notes` | optional text of at most 240 characters: counts and outcomes only |

Rules the checker enforces on cases: a case whose capability is not enabled must be `not_applicable`, and a case whose
capability is enabled must not be; `not_applicable` is refused for every other case.

### `latency.<series>`

The series are `trigger_to_ready.cold_launch`, `trigger_to_ready.warm_switch`, `trigger_to_ready.locked_handoff`,
`trigger_to_ready.shortcut_cold`, `trigger_to_ready.shortcut_warm`, `trigger_to_ready.text_field_cold`, `trigger_to_ready.text_field_warm`, `end_of_input_to_save.voice` and
`end_of_input_to_save.text`. Each entry:

| Key | Value |
| --- | --- |
| `sample_count` | integer, at least 5 |
| `median_ms`, `p95_ms`, `min_ms`, `max_ms` | non-negative numbers, `min_ms <= median_ms <= p95_ms <= max_ms` |
| `second_source_artifact` | optional artifact name of the F67 in-app metrics export, when the candidate has it |

Nothing else from the recordings is kept: no per-trial list.

### `issues[]`

| Key | Value |
| --- | --- |
| `severity` | `low`, `medium`, `high` or `critical` |
| `summary` | short text with no capture content |
| `follow_up_task` | the follow-up task key, or |
| `acceptance` | the explicit acceptance for the trial; exactly one of the two |

An issue that loses an acknowledged capture, duplicates or invents a reminder, or leaks private content to locked access is
a `failed` case, not an issue.

## What a record never holds

The scan below is a best-effort guard, not proof. No capture text, and no team, profile, certificate or device identifier, and no hash of one. The checker refuses a value
that looks like a device UDID, a UUID or a hex run of 32 or more digits (the `revision` is checked by name). The device is
recorded by model and iOS version only.

## Example (synthetic, abbreviated)

```json
{
  "schema": "ohand.device-matrix.v1",
  "collected_at": "2026-11-12T18:40:00Z",
  "candidate_record": "docs/releases/m1-trial/candidate-412-0123abcd.json",
  "build_number": "412",
  "revision": "0123abcd0123abcd0123abcd0123abcd0123abcd",
  "device": { "model": "iPhone 16 Pro", "ios_version": "26.6.2" },
  "capabilities_enabled": [],
  "cases": {
    "offline-1": { "outcome": "passed", "artifacts": ["offline-1-01.png", "offline-1-02.png"],
                   "observed_at": "2026-11-10T09:15:00-08:00", "notes": "2 saved, 2 waiting" },
    "state-6": { "outcome": "not_applicable", "reason": "Mac inlet not included in candidate" },
    "config-4": { "outcome": "not_applicable", "reason": "capability not enabled in candidate" }
  },
  "latency": {
    "trigger_to_ready.cold_launch": { "sample_count": 6, "median_ms": 950, "p95_ms": 1150, "min_ms": 830, "max_ms": 1180 }
  },
  "private_evidence_reference": "maintainer's private folder for this candidate, kept on the maintainer's Mac",
  "issues": []
}
```

The example lists only some cases and one series so that it stays short; a real record needs every case and every series,
or the checker refuses it. The tests build a complete synthetic record (`tools/validation/device_matrix/test_check_device_matrix.py`).
