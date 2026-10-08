# M1 Intent Fixture Schema

Schema and semantics of `fixtures/intent/contrastive-fixtures.json`, the synthetic oracle for the M1 interpretation contract (I01 proposals, I02 time resolution). `core/tests/fixture_validation/main.rs` enforces everything below; run it with `cargo test --locked -p ohand-core --test fixture_validation` (also part of `make test`). The fixture list and rationale are in [intent-fixtures.md](../validation/intent-fixtures.md).

The file is `{"fixtures": [...]}`. Every object below rejects unknown keys and duplicate keys. Absent and `null` mean the same thing.

## Fixture

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | lowercase kebab-case string | Unique. Must be listed in `docs/validation/intent-fixtures.md`. |
| `category` | enum | `design`, `dates`, `mixed`, `corrections`, `ambiguity`, `broad-intention`, `negation` (negation and quoted speech), `prompt-injection`, `dropped-asr-word`. Every category must have a fixture. |
| `input` | string | The raw capture or transcript, untrusted data. Always preserved verbatim. |
| `provenance` | string | Must start with `synthetic`. No fixture comes from live account traffic. |
| `text_basis` | `original` or `corrected` | The text every span indexes into and the proposal's `TextBasis` (`Original` or `Correction`). Spans count Unicode scalar values. `corrected` exactly when `capture_context.user_correction` exists. |
| `preserve` | `{raw_input, user_correction}` | Storage invariants. `raw_input` is always `true` (raw captures are never overwritten). `user_correction` is `true` exactly when a correction exists, meaning both raw and corrected text are kept. |
| `capture_context` | object | See below. |
| `expected` | object | The reference outcome, below. |
| `forbidden` | object | Outcomes that are always wrong, below. At least one rule per fixture. |
| `recoverable` | bool | `false` means the transcript alone lacks the information needed to recover what the user meant (dropped words, garbled or missing time, quoted speech, no content, an unsupported operation). The expected outcome is then the conservative one. |
| `recovery_notes` | string | Required exactly when `recoverable` is `false`: what is missing. |
| `notes` | string | Why the expected and forbidden values are right. |

`capture_context`: `capture_type` (`text` or `voice`), optional `notes`, and:

- `capture_instant` (RFC 3339) and `device_timezone` (IANA): the I02 time context. Required whenever `expected.reminder_proposal` exists, so every instant can be recomputed.
- `user_correction` (string) and `correction_spans` (`[{original, corrected}]`): the user's corrected text. Applying each `original`→`corrected` replacement to `input` must produce `user_correction` exactly.

## Expected (full match)

`expected` holds the facets of the reference proposal. It is a full match: a facet that is absent must also be absent in the evaluated proposal. It uses the I01 names and serde forms, and the test rebuilds it as a real `Proposal` and requires `Proposal::validate` to pass, so it obeys every I01 rule (evidence spans for `item_type`, a span on every reminder and topic, abstention excludes all facets, no instant on an ambiguous reminder, an RFC 3339 instant plus IANA zone on a resolved one).

- `item_type`: `broad_intention`, `note`, `idea` or `action`; needs `source_spans`.
- `source_spans`: evidence for the item type, `[{start, end, text}]`.
- `reminder_proposal`: `{quality, instant, timezone_id, source_span}`; `source_span` is the time phrase only, not "Remind me". A reminder requires `item_type: action` (reminders attach only to an active action), and the action's `source_spans` must name the reminder target: after removing the reminder time phrase and the command words `remind`, `me`, `maybe`, `please`, `to`, `on`, `at`, `by`, `in`, `for` and `about`, some evidence span must still contain a word. "Remind me Friday at 10 a.m." therefore fails; "Remind me Friday at 10 a.m. to call the plumber" passes.
- `session_topic_proposal`: `{topic, source_span}`.
- `abstention`: `"UncertainTarget"`, `"Negated"`, `"Ambiguous"`, `"UnsupportedOperation"` or `{"Other": "reason"}` (I01's serde form). Only the variant is compared; the `Other` text is free. An abstention means the capture's item still exists with the source preserved, but no derived facet is applied.

`text` in a span is oracle-only: the exact slice `basis[start..end]`, checked by the test. Evaluators compare spans by overlap, not equality.

### Time semantics

- `explicit`: date and hour are both stated. A device-default timezone does not lower this.
- `inferred`: reserved for a time with a stated component filled by a documented default. No M1 fixture needs it, but it is legal and forbidden where it would be wrong.
- `ambiguous`: no instant, no timezone. A date without an hour ("Remind me Friday") is `ambiguous`, matching I02 `MissingHour`; a midnight or default hour is never invented. Hedged or garbled phrases and an abbreviation that conflicts with the device zone (EST on an EDT date) are also `ambiguous`.
- A deadline or dated fact without a reminder request ("finalize by next Friday") is never a reminder.

The test recomputes every resolved instant: its UTC offset must be the one the named zone observes, it must be after `capture_instant`, and the phrase's weekday, "today"/"tomorrow" and `N a.m./p.m.` hour must match the local result.

## Forbidden rules

Each key is a rule; an evaluated proposal violates `forbidden` if it matches any of them. The expected outcome must violate none (checked).

| Key | Value | Violated when the proposal... |
| --- | --- | --- |
| `item_types` | list of item types | has one of these `item_type`s. |
| `reminder` | `"any"` | has any `reminder_proposal`. |
| | `"any_instant"` | has a reminder with a resolved `instant` (an ambiguous reminder without one is allowed). |
| `reminder_qualities` | list of `explicit`/`inferred`/`ambiguous` | has a reminder of one of these qualities. |
| `reminder_timezones` | list of IANA zones | has a reminder in one of these zones. |
| `session_topic` | `true` | has a `session_topic_proposal`. |
| `facets` | `"any"` | has any `item_type`, reminder or topic (only an abstention is acceptable). |
| `operations` | list of `update`/`create` | uses that operation, unless it is the I01-legal degenerate form: abstention `UnsupportedOperation` and no facets. |

A fixture that expects "no target mutation" (an unsupported spoken update such as "Done with the roofer call") expects `UnsupportedOperation`, forbids `facets: "any"` and `operations: ["update", "create"]`, and keeps `preserve.raw_input`.

## Validation performed

1. Strict typed parse (unknown or duplicate keys, bad enum values and missing required fields fail).
2. Unique kebab-case ids; `synthetic` provenance; labels (`recoverable`/`recovery_notes`), `preserve` and `text_basis` consistency; correction replay.
3. Span text equals the basis slice and is in bounds.
4. A `reminder_proposal` is accompanied by `item_type: action` whose evidence extends beyond the time phrase.
5. The expected facets rebuilt as an I01 `Proposal` pass `Proposal::validate` (abstention exclusivity, evidence, RFC 3339, IANA zone, ambiguous reminder without instant).
6. Reminder instants recomputed against the time context.
7. Expected does not violate its own `forbidden`; each forbidden rule is exercised against synthetic violating proposals.
8. Corpus coverage: all categories, the DESIGN examples, the unsupported-update, negation, injection, correction and dropped-ASR requirements.
9. Mutation tests: bad vocabulary, extra keys, broken evidence, wrong instants and dropped labels are each rejected with the expected error.

## Maintenance

Add fixtures with an exact `text` for every span, a full time context for reminders, a concrete `forbidden` rule, and an entry in `docs/validation/intent-fixtures.md`. No fixture claims a particular LLM agrees with it. Never add recorded or live-account content.
