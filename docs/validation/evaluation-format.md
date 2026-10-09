# Semantic Evaluation Reports

Status: M1 E01. The `evaluation` crate (`tools/evaluation`, binary `evaluate`) runs the synthetic intent corpus through the real interpretation pipeline and reports what it did, against the oracle in each fixture. It is a measurement and safety tool. It does not produce an accuracy score, and nothing it prints says how a live model behaves.

- Corpus (oracle): [`fixtures/intent/contrastive-fixtures.json`](../../fixtures/intent/contrastive-fixtures.json), described in [intent-fixtures.md](intent-fixtures.md) and [fixture-schema.md](../features/fixture-schema.md).
- Provider replies: [`tools/evaluation/fixtures/recorded-responses.json`](../../tools/evaluation/fixtures/recorded-responses.json).
- Ownership: the crate, its recorded replies and this document belong to E01. The only shared-file change is the one-line `tools/evaluation` workspace-member entry in `Cargo.toml` (and the `Cargo.lock` entries it implies), without which `cargo test --all` would not run the crate; see the Shared File Ordering note in [m1-contracts.md](../architecture/m1-contracts.md).
- Enforcement: `tools/evaluation/tests/evaluation.rs` and the unit tests in `tools/evaluation/src/run.rs`, run by `cargo test --all --locked` (part of `make test`); the crate is built, formatted and linted by `make check` like every workspace member.

```
cargo run -p evaluation -- --format text            # human summary
cargo run -p evaluation -- --format json --out r.json
cargo run -p evaluation -- --no-responses           # deterministic run only
```

The exit status is 1 when the gate fails, 2 for unusable input, 0 otherwise. `--no-gate` prints the report and always exits 0 for a readable result; CI does not use it.

## What is executed

For every case the tool creates a fresh migrated SQLite store in a temporary directory (nothing real is ever opened), saves the fixture's capture and, when the fixture's text basis is `corrected`, its text correction, creates the item, and enqueues and leases an on-device interpretation job for the current revision. A candidate proposal is then submitted to the production `apply_interpretation_proposal` (a provider failure to `record_interpretation_failure`), and the durable state is read back. Identifiers are derived from the case name, so reruns are identical.

| Execution | Producer of the candidate | Status here |
| --- | --- | --- |
| `deterministic` | `recognize_with_session_topic`, the offline fast path, with no provider | executed for every fixture |
| `fake` | a scripted reply through `FakeProvider`, `dispatch` and `InterpretationMapping::map_output` | executed for scenarios with `synthetic_authored` provenance |
| `recorded` | the same path, for replies captured from a specific authorized live run | executed only for scenarios with complete `live_run` provenance; none exist in the repository |
| `live` | a call to a real provider | always `unavailable`; this tool makes no network call |

An execution kind that did not run is listed with `"status": "unavailable"` and a reason, has zero cases, appears in `gate.unavailable`, and is never counted as passed. The fast path handles a narrow grammar, so most fixtures yield `not_handled` in the deterministic run. That is reported as its own count (`producer.not_handled`, `not_handled_where_oracle_has_facets`); it is neither a pass nor a defect.

## Provenance of recorded replies

Every scenario names its `fixture_id`, a `role` (`conforming` replays the oracle as a well-formed reply; `adversarial` is an untrusted or malformed reply meant to test the guard), a `description`, a `provenance` and a `behavior` (`respond` with a JSON object body, `respond_raw` with arbitrary bytes, or `transport_error`).

- `{"kind": "synthetic_authored"}`: authored in this repository. Executed and reported as `fake`. This is the only provenance the repository's file carries.
- `{"kind": "live_run", "run_id", "authorized_by", "collected_at", "provider_profile"}`: a reply collected in a specific authorized live run against synthetic corpus inputs. All four fields are required (`collected_at` is RFC 3339) and checked on load; a partial claim is a load error, not a downgrade. Only this makes a scenario `recorded`.

A conforming synthetic scenario passing, or the whole fake run passing, is evidence about this repository's mapping and guard. It is not evidence about any model, and no report field infers one.

## Report contents

The JSON report has no timestamps, paths or durations and uses ordered maps, so two runs over the same inputs are byte-identical. Its header records `report_format_version`, the `instruction_version` (hash of the published M1 instructions), the corpus path, its `version` (`sha256:` of the exact file bytes), the fixture count and provenance (`synthetic`; a corpus with any non-synthetic fixture is rejected), and the same version information for the responses file.

Each run has `totals` and per-case records. The counts are findings and cases, each with its denominator beside it (`cases`, `oracle_abstains`, `oracle_has_action`, `oracle_has_resolved_instant`, `oracle_has_no_reminder`). No ratio, percentage or composite figure exists, by design: agreement with an oracle that the fixtures themselves define would say little, and a rate would let a small safety failure hide behind many easy cases.

Separate counts, each at two stages, `candidate` (what the interpreter proposed) and `authoritative` (what survived the guard into the store):

| Defect | Meaning |
| --- | --- |
| `false_action` | `action` where the oracle has another type or none |
| `false_deadline` | a resolved reminder instant the oracle does not support |
| `false_completion` | a request to change an existing item, including one the mapping rejected (see below); at the authoritative stage, a stored item that left the active state (completed, cancelled or deleted), which is also a `lifecycle` forbidden hit |
| `wrong_item_type` | an item type other than the oracle's, or one where the oracle has none (`action` is `false_action` instead) |
| `unsupported_claim` | any other topic, quality, zone, reminder or request to create another item the oracle lacks, each item-type evidence span that overlaps none of the oracle's spans, and reminder or session-topic evidence that overlaps none of the oracle's corresponding span |
| `missed_intent` | a facet the oracle has that is absent |

Class-level failures are reported per fixture class. Each run carries `by_category`: the same totals (`cases` and the oracle denominators, defects at both stages, abstentions, forbidden hits at both stages) for every fixture category the run covered (`design`, `dates`, `mixed`, `corrections`, `ambiguity`, `broad-intention`, `negation`, `prompt-injection`, `dropped-asr-word`), and the text report prints a `class <name>` block for each. A failure confined to one class therefore shows up in that class and not only in a run-wide count. A class a run did not cover is absent, not zero.

A reply the instruction mapping refuses because it asks to `update` an existing item or to `create` another one is still a finding: the producer outcome is `mapping_rejected` with `requested_operation`, and the request is scored at the candidate stage (`false_completion` for an update, `unsupported_claim` for a create, plus `operation` forbidden hits where the fixture forbids them). The guard still receives an invalid-output failure, so the authoritative stage is unaffected.

Abstentions are counted apart: `correct`, `wrong_reason` (a different reason variant), `missed` (the oracle abstains and the interpreter does not) and `unexpected`. `forbidden_*` counts forbidden-rule hits per mutation class (`classification`, `reminder`, `session_topic`, `operation`, `lifecycle`, `source_text`). Producer outcomes (`proposed`, `abstained`, `not_handled`, `provider_failure`, `mapping_rejected`) and the guard's verdicts (`applied/<reminder disposition>`, `abstained`, `failed_permanently`, `retry_later`, `duplicate`, `rejected`) are counted too. A reply rejected by the mapping is recorded as an invalid-output failure and is not scored as an interpretation; a provider outage leaves the capture saved and the job retriable, with no defect.

Item-type evidence is checked span by span: every proposed span must overlap one of the oracle's spans, so an unrelated span beside a valid one is reported. Reminder and session-topic evidence is compared with the oracle's span for that same facet. These checks are independent: a response with several defects counts toward each class it breaks, so a false deadline does not hide unrelated reminder evidence and a wrong topic does not hide unrelated topic evidence. The one exception is a reminder the oracle lacks entirely, which is a single defect: `false_deadline` when it carries an instant, `unsupported_claim` otherwise. At the authoritative stage the evidence a stored facet rested on is carried over from the candidate, since the store keeps none.

At the authoritative stage the store holds no resolution quality, so quality is compared only for candidates. Time zones are compared at the authoritative stage only for a reminder that carries an instant; a stored not-scheduled reminder shows the capture's zone for display and schedules nothing.

## The gate

`gate.name` is `zero-forbidden-authoritative-mutations`. It passes when, summed over every executed run, no authoritative state breaks a fixture's `forbidden` rules, and additionally, for every fixture, when the item left the `active` state, the raw capture or the user's correction no longer equals what was saved, or the store holds any item, capture, event, correction or reminder row beyond the evaluated item's own (`authoritative_state.foreign_records`; reported as an authoritative `operation` hit, because an accepted `create` or `update` would leave exactly that). `an_extra_item_in_durable_state_is_an_authoritative_operation_hit_that_fails_the_gate` plants such a row and checks that the gate fails. It also fails if the deterministic run did not execute or an executed run has no cases. Candidate-stage hits are reported but do not fail the gate: they are what the guard had to stop.

The harness checks the actual stored state, so the gate depends on what the guard does, not on what a reply was meant to do. In the repository's scenarios the guard keeps out every forged reminder (injection, reported speech, wrong instant, hedged or ambiguous time promoted to an instant, wrong zone), rejects update and create operations, provenance fields and unknown fields at the mapping, and fails closed on prose, malformed evidence and transport errors.

Known limits, stated plainly:

- The guard cannot judge a model-owned label. An `item_type` or session topic that passes validation is stored. If a reply labels a prompt-injection capture `action`, the report counts a `classification` forbidden mutation and the gate fails (`a_forbidden_label_reaching_durable_state_fails_the_gate` pins this). The repository's scenarios contain no such reply, so the gate passes; that says the guard is sound for the replies it checks, not that a live model will not mislabel.
- The corpus is small and synthetic. Zero hits over 33 fixtures and the authored scenarios is a regression gate, not a safety proof.
- At the time of writing the time resolver does not parse several explicit phrases that the oracle expects to resolve (for example `Friday at 3 p.m.`). The guard then stores a not-scheduled reminder with the reason "the time phrase could not be understood", which the report shows as `missed_intent` at the authoritative stage. That is a safe failure and a real finding, not a forbidden mutation.

## Adding scenarios or a live run

Add scenarios to `tools/evaluation/fixtures/recorded-responses.json`; a scenario for a fixture the corpus lacks, a duplicate `scenario_id`, an empty `description` or a non-object `respond` body is a load error. To add recorded replies from a live run, only an operator who holds the authorization may add them, with `live_run` provenance and synthetic inputs only; this tool never contacts a provider, so it never produces a `live` result. Changing the corpus changes `corpus.version`; reports with different versions are not comparable.
