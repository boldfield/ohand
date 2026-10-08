use serde_json::Value;
use std::fs;
use std::path::PathBuf;

mod fixture_tests {
    use super::*;

    /// Get the path to fixtures directory relative to the project root.
    fn fixture_path() -> PathBuf {
        let manifest = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest)
            .parent()
            .unwrap()
            .join("fixtures/intent/contrastive-fixtures.json")
    }

    /// Load fixtures from the JSON file.
    fn load_fixtures() -> Vec<Value> {
        let path = fixture_path();
        let content =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to load fixtures from {:?}: {}", path, e));
        let json: Value = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("Failed to parse fixtures JSON: {}", e));
        json["fixtures"]
            .as_array()
            .expect("fixtures must be an array")
            .clone()
    }

    /// Check that each fixture has required fields.
    fn validate_fixture_structure(fixture: &Value, index: usize) {
        let fixture_id = fixture["id"]
            .as_str()
            .unwrap_or_else(|| panic!("Fixture {} missing 'id'", index));

        // Required fields
        assert!(
            fixture.get("input").is_some(),
            "Fixture {} ({}) missing 'input'",
            index,
            fixture_id
        );
        assert!(
            fixture.get("provenance").is_some(),
            "Fixture {} ({}) missing 'provenance'",
            index,
            fixture_id
        );
        assert!(
            fixture.get("capture_context").is_some(),
            "Fixture {} ({}) missing 'capture_context'",
            index,
            fixture_id
        );
        assert!(
            fixture.get("expected").is_some(),
            "Fixture {} ({}) missing 'expected'",
            index,
            fixture_id
        );
        assert!(
            fixture.get("forbidden").is_some(),
            "Fixture {} ({}) missing 'forbidden'",
            index,
            fixture_id
        );
        assert!(
            fixture.get("notes").is_some(),
            "Fixture {} ({}) missing 'notes'",
            index,
            fixture_id
        );

        // recoverable should be present
        assert!(
            fixture.get("recoverable").is_some(),
            "Fixture {} ({}) missing 'recoverable' field",
            index,
            fixture_id
        );
    }

    /// Validate capture_context structure.
    fn validate_capture_context(fixture: &Value, index: usize) {
        let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
        let capture_context = &fixture["capture_context"];

        assert!(
            capture_context.get("capture_type").is_some(),
            "Fixture {} ({}) capture_context missing 'capture_type'",
            index,
            fixture_id
        );

        let capture_type = capture_context["capture_type"].as_str().unwrap();
        assert!(
            capture_type == "text" || capture_type == "voice",
            "Fixture {} ({}): capture_type must be 'text' or 'voice', got '{}'",
            index,
            fixture_id,
            capture_type
        );
    }

    /// Validate reminder_proposal structure.
    fn validate_reminder_proposal(reminder: &Value, context: &str) {
        if !reminder.is_null() && !reminder.is_object() {
            panic!("{}: reminder_proposal must be an object or null", context);
        }

        if let Some(obj) = reminder.as_object() {
            if let Some(quality) = obj.get("quality") {
                let q = quality.as_str().unwrap_or("");
                // "any" is a valid sentinel in forbidden sections
                assert!(
                    q == "explicit" || q == "inferred" || q == "ambiguous" || q == "any",
                    "{}: quality must be 'explicit', 'inferred', 'ambiguous', or 'any', got '{}'",
                    context,
                    q
                );

                // For ambiguous, instant must be absent
                if q == "ambiguous" {
                    assert!(
                        obj.get("instant").is_none() || obj.get("instant").unwrap().is_null(),
                        "{}: ambiguous reminder must not have instant",
                        context
                    );
                }

                // For explicit/inferred, instant is required (but not in forbidden section)
                if !context.ends_with("(forbidden)") && (q == "explicit" || q == "inferred")
                    && q != "any"
                {
                    assert!(
                        obj.get("instant").is_some() && !obj.get("instant").unwrap().is_null(),
                        "{}: quality '{}' requires instant",
                        context,
                        q
                    );
                }
            }

            // Validate source_span if present
            if let Some(span) = obj.get("source_span") {
                if let Some(span_obj) = span.as_object() {
                    assert!(
                        span_obj.contains_key("start") && span_obj.contains_key("end"),
                        "{}: source_span must have 'start' and 'end'",
                        context
                    );
                }
            }
        }
    }

    /// Validate abstention value.
    fn validate_abstention(abstention: &Value, context: &str) {
        if !abstention.is_null() {
            if let Some(reason) = abstention.as_str() {
                assert!(
                    reason == "uncertain-target"
                        || reason == "negated"
                        || reason == "ambiguous"
                        || reason == "unsupported-operation"
                        || reason.starts_with("other"),
                    "{}: unknown abstention reason '{}'",
                    context,
                    reason
                );
            }
        }
    }

    /// Validate source spans in expected/forbidden.
    fn validate_source_spans(spans: &Value, input: &str, context: &str) {
        if let Some(array) = spans.as_array() {
            for span in array {
                if let Some(obj) = span.as_object() {
                    if let (Some(start), Some(end)) = (obj.get("start"), obj.get("end")) {
                        let start_idx = start.as_u64().unwrap_or(0) as usize;
                        let end_idx = end.as_u64().unwrap_or(0) as usize;
                        let input_chars = input.chars().count();

                        assert!(
                            start_idx < end_idx,
                            "{}: source_span invalid [{}, {})",
                            context,
                            start_idx,
                            end_idx
                        );
                        assert!(
                            end_idx <= input_chars,
                            "{}: source_span [{}, {}) out of bounds (input has {} chars)",
                            context,
                            start_idx,
                            end_idx,
                            input_chars
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_fixture_files_exist() {
        let path = fixture_path();
        assert!(
            path.exists(),
            "fixtures/intent/contrastive-fixtures.json does not exist at {:?}",
            path
        );
    }

    #[test]
    fn test_fixture_json_valid() {
        let _fixtures = load_fixtures();
        // If we get here, fixtures loaded and parsed successfully
    }

    #[test]
    fn test_all_fixtures_have_required_structure() {
        let fixtures = load_fixtures();
        for (index, fixture) in fixtures.iter().enumerate() {
            validate_fixture_structure(fixture, index);
        }
    }

    #[test]
    fn test_all_fixtures_have_valid_capture_context() {
        let fixtures = load_fixtures();
        for (index, fixture) in fixtures.iter().enumerate() {
            validate_capture_context(fixture, index);
        }
    }

    #[test]
    fn test_reminder_proposal_structure() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");

            // Check expected reminder
            if let Some(reminder) = fixture["expected"].get("reminder_proposal") {
                validate_reminder_proposal(reminder, &format!("{} (expected)", fixture_id));
            }

            // Check forbidden reminder
            if let Some(reminder) = fixture["forbidden"].get("reminder_proposal") {
                validate_reminder_proposal(reminder, &format!("{} (forbidden)", fixture_id));
            }
        }
    }

    #[test]
    fn test_abstention_reasons_valid() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");

            // Check expected abstention
            if let Some(abstention) = fixture["expected"].get("abstention") {
                validate_abstention(abstention, &format!("{} (expected)", fixture_id));
            }

            // Check forbidden abstention
            if let Some(abstention) = fixture["forbidden"].get("abstention") {
                validate_abstention(abstention, &format!("{} (forbidden)", fixture_id));
            }
        }
    }

    #[test]
    fn test_source_spans_within_bounds() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let input = fixture["input"].as_str().unwrap_or("");

            // Check expected source_spans
            if let Some(spans) = fixture["expected"].get("source_spans") {
                validate_source_spans(spans, input, &format!("{} (expected)", fixture_id));
            }

            // Check expected reminder source_span
            if let Some(reminder) = fixture["expected"].get("reminder_proposal") {
                if let Some(span) = reminder.get("source_span") {
                    if let Some(obj) = span.as_object() {
                        if let (Some(start), Some(end)) = (obj.get("start"), obj.get("end")) {
                            let start_idx = start.as_u64().unwrap_or(0) as usize;
                            let end_idx = end.as_u64().unwrap_or(0) as usize;
                            let input_chars = input.chars().count();

                            assert!(
                                start_idx < end_idx,
                                "{}: reminder source_span invalid [{}, {})",
                                fixture_id,
                                start_idx,
                                end_idx
                            );
                            assert!(
                                end_idx <= input_chars,
                                "{}: reminder source_span [{}, {}) out of bounds (input has {} chars)",
                                fixture_id,
                                start_idx,
                                end_idx,
                                input_chars
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_all_required_categories_present() {
        let fixtures = load_fixtures();
        let ids: Vec<&str> = fixtures.iter().filter_map(|f| f["id"].as_str()).collect();

        // Check for representation of required categories
        let categories = vec![
            (
                "design",
                vec!["design-broad-intention", "design-dated-information"],
            ),
            (
                "mixed",
                vec!["mixed-idea-action-capture", "mixed-note-action-capture"],
            ),
            (
                "date",
                vec!["date-relative-next-monday", "date-explicit-today"],
            ),
            (
                "timezone",
                vec!["timezone-explicit", "timezone-implicit-local"],
            ),
            (
                "asr",
                vec!["asr-dropped-word-remind-me", "asr-garbled-time"],
            ),
            ("negation", vec!["negation-do-not-remind", "quoted-text"]),
            (
                "prompt-injection",
                vec!["prompt-injection-attempt-1", "prompt-injection-attempt-2"],
            ),
        ];

        for (category, examples) in categories {
            for example in examples {
                assert!(
                    ids.contains(&example),
                    "Missing required fixture category '{}': {}",
                    category,
                    example
                );
            }
        }
    }

    #[test]
    fn test_recoverable_field_present() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            assert!(
                fixture.get("recoverable").is_some(),
                "Fixture '{}' missing 'recoverable' field",
                fixture_id
            );

            let recoverable = fixture["recoverable"].as_bool();
            assert!(
                recoverable.is_some(),
                "Fixture '{}' recoverable must be a boolean",
                fixture_id
            );

            // If not recoverable, should have recovery_notes
            if recoverable == Some(false) {
                assert!(
                    fixture.get("recovery_notes").is_some(),
                    "Fixture '{}' is not recoverable but missing 'recovery_notes'",
                    fixture_id
                );
            }
        }
    }

    #[test]
    fn test_no_contradictions_in_expected_forbidden() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let expected = &fixture["expected"];
            let forbidden = &fixture["forbidden"];

            // Both shouldn't require a field with specific value
            // This is a basic check; comprehensive contradiction checking would be more complex

            // Check reminder quality doesn't contradict
            if let (Some(exp_rem), Some(forb_rem)) = (
                expected.get("reminder_proposal"),
                forbidden.get("reminder_proposal"),
            ) {
                if let (Some(exp_quality), Some(forb_quality)) =
                    (exp_rem.get("quality"), forb_rem.get("quality"))
                {
                    if exp_quality.as_str() != Some("any") && forb_quality.as_str() != Some("any") {
                        if let (Some(eq), Some(fq)) = (exp_quality.as_str(), forb_quality.as_str())
                        {
                            // If both specify specific quality, they shouldn't be the same
                            if eq == fq && eq != "any" {
                                panic!(
                                    "Fixture '{}': expected and forbidden both require quality '{}'",
                                    fixture_id, eq
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_fixture_ids_unique() {
        let fixtures = load_fixtures();
        let mut ids: Vec<String> = fixtures
            .iter()
            .filter_map(|f| f["id"].as_str().map(|s| s.to_string()))
            .collect();

        let original_len = ids.len();
        ids.sort();
        ids.dedup();

        assert_eq!(
            ids.len(),
            original_len,
            "Fixture IDs must be unique. Found duplicates."
        );
    }

    #[test]
    fn test_provenance_is_synthetic() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let provenance = fixture["provenance"].as_str().unwrap_or("");

            assert!(
                provenance.starts_with("synthetic"),
                "Fixture '{}' provenance must start with 'synthetic', got '{}'",
                fixture_id,
                provenance
            );
        }
    }
}
