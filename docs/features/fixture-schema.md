# M1 Intent Interpretation Fixture Schema

This document defines the structure and semantics of test fixtures in `fixtures/intent/contrastive-fixtures.json`. Fixtures are synthetic test cases used to validate the M1 interpretation proposal contract against the I01/I02 specifications.

## Fixture Structure

Each fixture is a JSON object with the following fields:

```json
{
  "id": "unique-fixture-identifier",
  "input": "The text or voice capture to be interpreted",
  "provenance": "synthetic, [source description]",
  "capture_context": {
    "capture_type": "text|voice",
    "capture_instant": "2026-10-08T14:00:00Z",
    "device_timezone": "America/New_York",
    "notes": "Optional context notes"
  },
  "expected": {
    "item_type": "action|note|idea|null",
    "abstention": "abstention-reason|null",
    "reminder_proposal": { ... },
    "session_topic_proposal": { ... }
  },
  "forbidden": { ... },
  "recoverable": true|false,
  "recovery_notes": "Optional explanation if recoverable=false",
  "notes": "Explanation of the fixture's intent"
}
```

## Field Definitions

### Top-Level Fields

- **id** (string, required): Unique identifier for the fixture, used for test reporting. Format: `category-description` (e.g., `date-relative-next-monday`).
- **input** (string, required): The source text or transcript to be interpreted. This is the untrusted data that the interpreter processes.
- **provenance** (string, required): Metadata describing the origin. Must be `synthetic, [source]` for all fixtures in this file (e.g., `synthetic, from DESIGN.md intent examples`).
- **capture_context** (object, required): Context metadata about the capture.
  - **capture_type** (string, required): `text` for written input, `voice` for transcribed speech.
  - **capture_instant** (string, optional): RFC 3339 instant when the capture was made. Required for fixtures with time resolution. Example: `2026-10-08T14:00:00Z`.
  - **device_timezone** (string, optional): IANA timezone identifier of the device. Used for inferring timezone when not explicit in input. Example: `America/New_York`.
  - **user_correction** (string, optional): Corrected transcription if the original input was from ASR. Present only for correction scenarios.
  - **correction_spans** (array, optional): Array of `{original: string, corrected: string}` objects marking which parts were corrected.
  - **notes** (string, optional): Free-form context about the capture.
- **expected** (object, required): The outcome the interpreter SHOULD produce.
  - **item_type** (`action|note|idea|null`, optional): The type of item proposed.
  - **abstention** (string, optional): Abstention reason if proposal does not proceed. Valid values: `uncertain-target`, `negated`, `ambiguous`, `unsupported-operation`, `other`, or `null`.
  - **reminder_proposal** (object, optional): Proposed reminder with I01 contract structure (see below).
  - **session_topic_proposal** (object, optional): Proposed session topic with contract structure (see below).
  - **source_spans** (array, optional): Array of `{start: number, end: number}` character offsets identifying evidence for the item_type.
- **forbidden** (object, required): Outcomes the interpreter MUST NOT produce. Structure mirrors `expected` but lists what is prohibited.
- **recoverable** (boolean, required): Whether the transcript contains enough information to confidently recover intent. Used to distinguish cases where transcript dropout or ambiguity makes intent unrecoverable.
- **recovery_notes** (string, optional): Explanation of why `recoverable: false`. Present only when `recoverable` is false.
- **notes** (string, required): Explanation of the fixture's purpose, the edge case it tests, and why the expected/forbidden outcomes are correct.

### Expected/Forbidden Structures

#### reminder_proposal

Maps to I01 `ReminderProposal`:

```json
{
  "quality": "explicit|inferred|ambiguous",
  "instant": "RFC 3339 timestamp",
  "timezone_id": "IANA timezone ID",
  "source_span": {
    "start": 0,
    "end": 27
  }
}
```

- **quality** (string, required): One of:
  - `explicit`: Unambiguous time from source (e.g., "3 p.m." or "tomorrow at 10 a.m.").
  - `inferred`: Resolvable with context (e.g., "next Monday" with a reference date).
  - `ambiguous`: Too unclear to resolve (e.g., "maybe Friday" or "fiveish").
- **instant** (string, optional): RFC 3339-formatted absolute instant. Required for `explicit` and `inferred` quality; must be absent for `ambiguous`.
- **timezone_id** (string, optional): IANA timezone identifier. Optional only when quality is `ambiguous`.
- **source_span** (object, optional): Character offset span in the input marking the time phrase. `{start: number, end: number}` in Unicode character positions.

#### session_topic_proposal

Maps to I01 `SessionTopicProposal`:

```json
{
  "topic": "string",
  "source_span": {
    "start": 0,
    "end": 7
  }
}
```

- **topic** (string, required): The proposed session topic string.
- **source_span** (object, optional): Character offset span marking the evidence.

## Testing and Validation

### Schema Validation

Fixtures are validated by a test suite that:

1. Parses each fixture as JSON.
2. Verifies required fields are present.
3. Checks field types match the schema.
4. Validates that `expected` and `forbidden` do not contain contradictions (e.g., expecting and forbidding the same value).
5. For time fixtures, ensures `source_span` character offsets are valid within the input.
6. Verifies that abstention is mutually exclusive with item_type and facet proposals.

### Semantic Validation

Fixtures must:

1. **Map to I01/I02 contract**: Expected outcomes must be expressible as valid I01 `Proposal` objects.
2. **Cover required categories**:
   - DESIGN examples (explicit intent semantics)
   - Mixed captures (multiple intents in one input)
   - Corrections (user-corrected transcriptions)
   - Ambiguity and uncertain targets (unrecoverable intent)
   - Broad intentions (exploratory, hypothetical, reflective)
   - Negation and quotation (preventing interpretation)
   - Prompt injection (treating source as untrusted data)
   - Date and timezone resolution (with various levels of clarity)
   - ASR errors (dropped words, garbled time)
3. **Provide complete TimeContext**: For fixtures expecting time resolution, `capture_instant` and `device_timezone` must be present so instants are reproducible.
4. **Document recoverability**: The `recoverable` flag indicates whether the transcript, by itself, contains enough information for a human to recover the intended meaning. This distinguishes recoverable ASR errors (clear intent despite dropout) from unrecoverable ones (intent is genuinely unclear).

## Category Reference

Fixtures are organized by testing category:

- **design-***: Scenarios from DESIGN.md intent examples.
- **mixed-***: Multiple intents in one capture.
- **correction-***: User-corrected transcriptions.
- **date-*, timezone-***: Time resolution with various clarity levels.
- **asr-***: Automatic speech recognition errors.
- **negation-*, quoted-***: Negation, quotation, hypothetical phrasing.
- **prompt-injection-***: Adversarial prompt injection attempts.
- **abstract-*, question-*, comparison-***: Broad exploratory intent.
- **already-completed-*, empty-or-noise***: Edge cases (past tense, filler).

## Example: Fixed Date Fixture

```json
{
  "id": "date-relative-next-monday",
  "input": "Remind me next Monday",
  "provenance": "synthetic, relative date",
  "capture_context": {
    "capture_type": "text",
    "capture_instant": "2026-10-08T14:00:00Z",
    "device_timezone": "America/New_York",
    "notes": "relative weekday reference; reference_date 2026-10-08 is Thursday"
  },
  "expected": {
    "reminder_proposal": {
      "quality": "inferred",
      "instant": "2026-10-12T00:00:00Z",
      "timezone_id": "America/New_York",
      "source_span": {
        "start": 10,
        "end": 21
      }
    }
  },
  "forbidden": {
    "reminder_proposal": {
      "quality": "ambiguous"
    }
  },
  "recoverable": true,
  "notes": "Reference date 2026-10-08 is Thursday; next Monday is 2026-10-12. Relative dates are inferred but resolvable. Quality is 'inferred', not 'ambiguous'."
}
```

## Fixture Maintenance

### Adding New Fixtures

1. Identify the decision point or edge case to test.
2. Choose an input that clearly exposes the case.
3. Set `capture_instant` and `device_timezone` for reproducible time resolution.
4. Define `expected` outcomes aligned with I01/I02.
5. Define `forbidden` outcomes (what must NOT happen).
6. Set `recoverable` accurately (would a human understand intent from this transcript alone?).
7. Use clear `notes` explaining the fixture's purpose.

### Updating Fixtures

Fixture updates must preserve `provenance: "synthetic"` and all category/identification fields. Only update `expected`, `forbidden`, or `notes` when:

- The I01/I02 contract changes and the fixture must align.
- The fixture had an error that testing discovered.
- A clarification is needed in the notes.

Document the reason for any update in the PR description.

## No Fixture Implies LLM Agreement

Each fixture is authored based on DESIGN.md intent examples, I01/I02 specifications, and property-based reasoning. No fixture is a claim that a particular LLM agrees with it—the fixtures define the baseline intent semantics for M1.
