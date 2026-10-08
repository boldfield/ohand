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
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixtures from {:?}: {}", path, e));
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
            fixture.get("category").is_some(),
            "Fixture {} ({}) missing 'category'",
            index,
            fixture_id
        );
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
    fn validate_reminder_proposal(
        reminder: &Value,
        context: &str,
        input: Option<&str>,
        fixture: Option<&Value>,
    ) {
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
                if !context.ends_with("(forbidden)")
                    && (q == "explicit" || q == "inferred")
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

            // Validate source_span: required for expected, optional for forbidden
            if !context.ends_with("(forbidden)") {
                assert!(
                    obj.get("source_span").is_some() && !obj.get("source_span").unwrap().is_null(),
                    "{}: expected reminder_proposal requires source_span",
                    context
                );
            }

            // Validate source_span structure and text if present
            if let Some(span) = obj.get("source_span") {
                if let Some(span_obj) = span.as_object() {
                    assert!(
                        span_obj.contains_key("start") && span_obj.contains_key("end"),
                        "{}: source_span must have 'start' and 'end'",
                        context
                    );

                    if let (Some(start), Some(end)) = (span_obj.get("start"), span_obj.get("end")) {
                        let start_idx = start.as_u64().unwrap_or(0) as usize;
                        let end_idx = end.as_u64().unwrap_or(0) as usize;

                        // Validate text if present
                        if let Some(expected_text) = span_obj.get("text") {
                            if let Some(text_str) = expected_text.as_str() {
                                // Check if this fixture has a user_correction; if so, use that text
                                let text_basis = if let Some(fixture_obj) = fixture {
                                    fixture_obj["capture_context"]["user_correction"].as_str()
                                } else {
                                    None
                                };

                                let actual_inp = text_basis.or(input).unwrap_or("");
                                let actual_text: String = actual_inp
                                    .chars()
                                    .skip(start_idx)
                                    .take(end_idx - start_idx)
                                    .collect();
                                assert_eq!(
                                    &actual_text, text_str,
                                    "{}: reminder source_span text mismatch",
                                    context
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Validate abstention value.
    fn validate_abstention(abstention: &Value, context: &str) {
        if !abstention.is_null() {
            if let Some(reason) = abstention.as_str() {
                assert!(
                    reason == "UncertainTarget"
                        || reason == "Negated"
                        || reason == "Ambiguous"
                        || reason == "UnsupportedOperation",
                    "{}: unknown abstention reason '{}'",
                    context,
                    reason
                );
            } else if let Some(obj) = abstention.as_object() {
                // Handle Other(String) format: {"Other": "reason"}
                assert!(
                    obj.contains_key("Other"),
                    "{}: abstention object must have 'Other' key",
                    context
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

                        // Validate text field if present
                        if let Some(expected_text) = obj.get("text") {
                            if let Some(text_str) = expected_text.as_str() {
                                let actual_text: String = input
                                    .chars()
                                    .skip(start_idx)
                                    .take(end_idx - start_idx)
                                    .collect();
                                assert_eq!(
                                    &actual_text, text_str,
                                    "{}: source_span [{}, {}) text mismatch. Expected '{}', got '{}'",
                                    context, start_idx, end_idx, text_str, actual_text
                                );
                            }
                        }
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

    /// Validate session_topic_proposal structure.
    fn validate_session_topic_proposal(proposal: &Value, context: &str, input: Option<&str>) {
        if !proposal.is_null() && !proposal.is_object() {
            panic!(
                "{}: session_topic_proposal must be an object or null",
                context
            );
        }

        if let Some(obj) = proposal.as_object() {
            assert!(
                obj.contains_key("topic"),
                "{}: session_topic_proposal must have 'topic'",
                context
            );

            // Require source_span for expected, optional for forbidden
            if !context.ends_with("(forbidden)") {
                assert!(
                    obj.get("source_span").is_some() && !obj.get("source_span").unwrap().is_null(),
                    "{}: expected session_topic_proposal requires source_span",
                    context
                );
            }

            // Validate source_span if present
            if let Some(span) = obj.get("source_span") {
                if let Some(span_obj) = span.as_object() {
                    assert!(
                        span_obj.contains_key("start") && span_obj.contains_key("end"),
                        "{}: source_span must have 'start' and 'end'",
                        context
                    );

                    if let (Some(start), Some(end), Some(inp)) =
                        (span_obj.get("start"), span_obj.get("end"), input)
                    {
                        let start_idx = start.as_u64().unwrap_or(0) as usize;
                        let end_idx = end.as_u64().unwrap_or(0) as usize;

                        // Validate text if present
                        if let Some(expected_text) = span_obj.get("text") {
                            if let Some(text_str) = expected_text.as_str() {
                                let actual_text: String = inp
                                    .chars()
                                    .skip(start_idx)
                                    .take(end_idx - start_idx)
                                    .collect();
                                assert_eq!(
                                    &actual_text, text_str,
                                    "{}: session_topic source_span text mismatch",
                                    context
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_reminder_proposal_structure() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let input = fixture["input"].as_str();

            // Check expected reminder
            if let Some(reminder) = fixture["expected"].get("reminder_proposal") {
                validate_reminder_proposal(
                    reminder,
                    &format!("{} (expected)", fixture_id),
                    input,
                    Some(fixture),
                );
            }

            // Check forbidden reminder
            if let Some(reminder) = fixture["forbidden"].get("reminder_proposal") {
                validate_reminder_proposal(
                    reminder,
                    &format!("{} (forbidden)", fixture_id),
                    input,
                    Some(fixture),
                );
            }
        }
    }

    #[test]
    fn test_session_topic_proposal_structure() {
        let fixtures = load_fixtures();
        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let input = fixture["input"].as_str();

            // Check expected session_topic
            if let Some(proposal) = fixture["expected"].get("session_topic_proposal") {
                validate_session_topic_proposal(
                    proposal,
                    &format!("{} (expected)", fixture_id),
                    input,
                );
            }

            // Check forbidden session_topic
            if let Some(proposal) = fixture["forbidden"].get("session_topic_proposal") {
                validate_session_topic_proposal(
                    proposal,
                    &format!("{} (forbidden)", fixture_id),
                    input,
                );
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

        // Collect fixtures by category field
        let mut categories_found: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        for fixture in fixtures.iter() {
            if let Some(cat) = fixture["category"].as_str() {
                let id = fixture["id"].as_str().unwrap_or("unknown").to_string();
                categories_found
                    .entry(cat.to_string())
                    .or_default()
                    .push(id);
            }
        }

        // Required categories per AC1
        let required_categories = vec![
            "design",
            "dates",
            "mixed",
            "corrections",
            "ambiguity",
            "broad-intention",
            "negation",
            "prompt-injection",
            "dropped-asr-word",
        ];

        for cat in required_categories {
            assert!(
                categories_found.contains_key(cat),
                "Missing required category '{}' from fixtures",
                cat
            );
            let fixtures_in_cat = &categories_found[cat];
            assert!(
                !fixtures_in_cat.is_empty(),
                "Category '{}' has no fixtures",
                cat
            );
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

    #[test]
    fn test_category_field_valid() {
        let fixtures = load_fixtures();
        let valid_categories = vec![
            "design",
            "dates",
            "mixed",
            "corrections",
            "ambiguity",
            "broad-intention",
            "negation",
            "prompt-injection",
            "dropped-asr-word",
        ];

        for fixture in fixtures.iter() {
            let fixture_id = fixture["id"].as_str().unwrap_or("unknown");
            let category = fixture["category"].as_str().unwrap_or("");

            assert!(
                valid_categories.contains(&category),
                "Fixture '{}' has invalid category '{}'. Valid: {:?}",
                fixture_id,
                category,
                valid_categories
            );
        }
    }
}
