mod proposal_tests {
    use ohand_core::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
    use ohand_core::interpretation::contracts::{
        AbstentionReason, Proposal, ReminderProposal, SessionTopicProposal, SourceSpan, TextBasis,
        TimeResolutionQuality,
    };
    use ohand_core::store::events::ItemType;

    #[test]
    fn test_text_basis_original_roundtrip() {
        let basis = TextBasis::Original;
        let (kind, id) = basis.to_db();
        assert_eq!(kind, "original");
        assert_eq!(id, None);

        let parsed = TextBasis::from_db(kind, id.as_deref()).expect("parse failed");
        assert_eq!(parsed, basis);
    }

    #[test]
    fn test_text_basis_correction_roundtrip() {
        let correction_id = "corr-123".to_string();
        let basis = TextBasis::Correction {
            correction_id: correction_id.clone(),
        };
        let (kind, id) = basis.to_db();
        assert_eq!(kind, "correction");
        assert_eq!(id, Some(correction_id.clone()));

        let parsed = TextBasis::from_db(kind, id.as_deref()).expect("parse failed");
        assert_eq!(parsed, basis);
    }

    #[test]
    fn test_text_basis_invalid_kind() {
        let result = TextBasis::from_db("unknown", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_source_span_valid_within_bounds() {
        let text = "Hello, World!";
        let span = SourceSpan::new(0, 5);
        assert!(span.is_valid(text).is_ok());
    }

    #[test]
    fn test_source_span_out_of_bounds() {
        let text = "Hello";
        let span = SourceSpan::new(0, 10);
        assert!(span.is_valid(text).is_err());
    }

    #[test]
    fn test_proposal_creation_minimal() {
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        );

        assert_eq!(proposal.proposal_id, "prop-1");
        assert_eq!(proposal.item_id, "item-1");
        assert_eq!(proposal.capture_id, "cap-1");
        assert_eq!(proposal.source_revision, 0);
        assert_eq!(proposal.schema_version, SUPPORTED_PROPOSAL_SCHEMA_VERSION);
        assert_eq!(proposal.item_type, None);
        assert_eq!(proposal.reminder_proposal, None);
        assert_eq!(proposal.session_topic_proposal, None);
        assert!(proposal.abstention.is_none());
    }

    #[test]
    fn test_proposal_validate_schema_version_supported() {
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate_schema_version();
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_schema_version_unsupported() {
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            999,
            TextBasis::Original,
            "req-1".to_string(),
        );

        let result = proposal.validate_schema_version();
        assert!(result.is_err());
    }

    #[test]
    fn test_proposal_validate_source_spans_valid() {
        let text = "Call the roofer on Friday";
        let spans = vec![
            SourceSpan::new(0, 4),   // "Call"
            SourceSpan::new(9, 15),  // "roofer"
            SourceSpan::new(19, 25), // "Friday"
        ];

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_source_spans(Some(spans.clone()))
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate_source_spans(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_reminder_proposal_valid() {
        let text = "Call the roofer on Friday";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(19, 25)), // "Friday"
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate_reminder_proposal(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_full_valid_with_all_fields() {
        let text = "Call the roofer on Friday";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(19, 25)), // "Friday"
        };
        let spans = vec![SourceSpan::new(0, 4), SourceSpan::new(19, 25)]; // "Call" and "Friday"

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action))
        .with_reminder_proposal(Some(reminder))
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "work".to_string(),
            source_span: Some(SourceSpan::new(9, 15)), // "roofer"
        }))
        .with_source_spans(Some(spans));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_type_only() {
        let text = "Maybe a roof garden would be nice";
        let spans = vec![SourceSpan::new(0, 5)]; // "Maybe"

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Idea))
        .with_source_spans(Some(spans));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_abstained() {
        let text = "Something unclear";

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Ambiguous));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_empty_fails() {
        let text = "Some text";

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        );

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("at least one field"));
    }

    #[test]
    fn test_reminder_time_resolution_quality_display() {
        assert_eq!(TimeResolutionQuality::Explicit.as_str(), "explicit");
        assert_eq!(TimeResolutionQuality::Inferred.as_str(), "inferred");
        assert_eq!(TimeResolutionQuality::Ambiguous.as_str(), "ambiguous");
    }

    #[test]
    fn test_reminder_time_resolution_quality_parse() {
        let result: Result<TimeResolutionQuality, _> = "explicit".parse();
        assert_eq!(
            result.expect("parse explicit"),
            TimeResolutionQuality::Explicit
        );

        let result: Result<TimeResolutionQuality, _> = "inferred".parse();
        assert_eq!(
            result.expect("parse inferred"),
            TimeResolutionQuality::Inferred
        );

        let result: Result<TimeResolutionQuality, _> = "ambiguous".parse();
        assert_eq!(
            result.expect("parse ambiguous"),
            TimeResolutionQuality::Ambiguous
        );

        let result: Result<TimeResolutionQuality, _> = "unknown".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_proposal_with_correction_basis() {
        let correction_id = "corr-456".to_string();
        let basis = TextBasis::Correction {
            correction_id: correction_id.clone(),
        };

        let proposal = Proposal::new(
            "prop-2".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            1,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            basis,
            "req-2".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        assert_eq!(proposal.source_revision, 1);
        assert!(matches!(proposal.text_basis, TextBasis::Correction { .. }));
    }

    #[test]
    fn test_proposal_uncertain_reminder_time() {
        let text = "Maybe remind me on Friday?";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(19, 25)), // "Friday"
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_negation_abstains() {
        let text = "Don't remind me about the roof";

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Negated));

        let result = proposal.validate(text);
        assert!(result.is_ok());
        assert!(proposal.abstention.is_some());
        assert!(matches!(
            proposal.abstention,
            Some(AbstentionReason::Negated)
        ));
    }

    // === Acceptance Criterion 1: Character Offsets and Bounds ===

    #[test]
    fn test_source_span_end_of_text_empty_rejected() {
        let text = "abc";
        let span = SourceSpan::new(3, 3);
        assert!(span.is_valid(text).is_err());
    }

    #[test]
    fn test_source_span_multibyte_characters() {
        let text = "héllo"; // 5 characters (h, é, l, l, o)
                            // Character span [0, 5) should be valid
        let span = SourceSpan::new(0, 5);
        assert!(span.is_valid(text).is_ok());

        // Character span [0, 6) should be invalid (out of bounds)
        let span_oob = SourceSpan::new(0, 6);
        assert!(span_oob.is_valid(text).is_err());
    }

    #[test]
    fn test_source_span_start_greater_than_end() {
        let text = "Hello";
        let span = SourceSpan::new(5, 3);
        assert!(span.is_valid(text).is_err());
    }

    #[test]
    fn test_source_span_out_of_bounds_high() {
        let text = "Hello";
        let span = SourceSpan::new(0, 10);
        assert!(span.is_valid(text).is_err());
    }

    // === Acceptance Criterion 2: Reminder Proposal Validation ===

    #[test]
    fn test_reminder_invalid_rfc3339_instant() {
        let text = "remind me";
        let reminder = ReminderProposal {
            instant: "not a time".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: None,
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("RFC 3339"));
    }

    #[test]
    fn test_reminder_invalid_iana_timezone() {
        let text = "remind me";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "Nowhere/Fake".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: None,
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("IANA timezone"));
    }

    #[test]
    fn test_reminder_source_span_out_of_bounds() {
        let text = "abc";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 9999)),
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of bounds"));
    }

    // === Acceptance Criterion 3: Abstention Exclusivity ===

    #[test]
    fn test_abstention_rejects_item_type() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Ambiguous))
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot coexist"));
    }

    #[test]
    fn test_abstention_rejects_reminder() {
        let text = "Some text";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 4)), // "Some"
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Ambiguous))
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot coexist"));
    }

    #[test]
    fn test_abstention_rejects_session_topic() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Ambiguous))
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "work".to_string(),
            source_span: Some(SourceSpan::new(0, 4)),
        }));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot coexist"));
    }

    // === Identifier Validation ===

    #[test]
    fn test_empty_proposal_id_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("proposal_id"));
    }

    #[test]
    fn test_empty_item_id_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("item_id"));
    }

    #[test]
    fn test_empty_capture_id_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("capture_id"));
    }

    #[test]
    fn test_empty_request_version_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("request_version"));
    }

    // === Session-Topic with Source Evidence ===

    #[test]
    fn test_session_topic_requires_source_span() {
        let text = "meeting notes";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "work".to_string(),
            source_span: None,
        }));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("source evidence"));
    }

    #[test]
    fn test_session_topic_with_source_span_valid() {
        let text = "meeting notes";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "work".to_string(),
            source_span: Some(SourceSpan::new(0, 7)), // "meeting"
        }));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    // === JSON Serde Deserialization with Unknown Fields ===

    #[test]
    fn test_json_unknown_fields_rejected() {
        let json = r#"{
            "proposal_id": "prop-1",
            "item_id": "item-1",
            "capture_id": "cap-1",
            "source_revision": 0,
            "schema_version": 1,
            "text_basis": "Original",
            "request_version": "req-1",
            "item_type": "Action",
            "unknown_field": "should fail"
        }"#;

        let result: serde_json::Result<Proposal> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_json_valid_proposal_deserializes() {
        let json = r#"{
            "proposal_id": "prop-1",
            "item_id": "item-1",
            "capture_id": "cap-1",
            "source_revision": 0,
            "schema_version": 1,
            "text_basis": "Original",
            "request_version": "req-1",
            "item_type": "action",
            "reminder_proposal": null,
            "session_topic_proposal": null,
            "source_spans": null,
            "abstention": null
        }"#;

        let result: serde_json::Result<Proposal> = serde_json::from_str(json);
        assert!(result.is_ok());
        let proposal = result.unwrap();
        assert_eq!(proposal.proposal_id, "prop-1");
    }

    // === Abstention Reason Variants ===

    #[test]
    fn test_abstention_reason_uncertain_target() {
        let text = "unclear what this is";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::UncertainTarget));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_abstention_reason_negated() {
        let text = "don't do this";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Negated));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_abstention_reason_ambiguous() {
        let text = "maybe later";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Ambiguous));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_abstention_reason_unsupported_operation() {
        let text = "recurring reminder";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::UnsupportedOperation));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_abstention_reason_other() {
        let text = "something custom";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_abstention(Some(AbstentionReason::Other("custom reason".to_string())));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    // === Nested Unknown Fields Rejection ===

    #[test]
    fn test_json_nested_unknown_fields_in_reminder_proposal() {
        let json = r#"{
            "proposal_id": "prop-1",
            "item_id": "item-1",
            "capture_id": "cap-1",
            "source_revision": 0,
            "schema_version": 1,
            "text_basis": "Original",
            "request_version": "req-1",
            "reminder_proposal": {
                "instant": "2026-10-10T15:00:00Z",
                "timezone_id": "America/New_York",
                "quality": "explicit",
                "source_span": {"start": 0, "end": 5},
                "evil_extra": "should fail"
            },
            "session_topic_proposal": null,
            "source_spans": null,
            "abstention": null
        }"#;

        let result: serde_json::Result<Proposal> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_json_nested_unknown_fields_in_source_span() {
        let json = r#"{
            "proposal_id": "prop-1",
            "item_id": "item-1",
            "capture_id": "cap-1",
            "source_revision": 0,
            "schema_version": 1,
            "text_basis": "Original",
            "request_version": "req-1",
            "reminder_proposal": {
                "instant": "2026-10-10T15:00:00Z",
                "timezone_id": "America/New_York",
                "quality": "explicit",
                "source_span": {"start": 0, "end": 5, "bogus": 1}
            },
            "session_topic_proposal": null,
            "source_spans": null,
            "abstention": null
        }"#;

        let result: serde_json::Result<Proposal> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_json_nested_unknown_fields_in_text_basis_correction() {
        let json = r#"{
            "proposal_id": "prop-1",
            "item_id": "item-1",
            "capture_id": "cap-1",
            "source_revision": 0,
            "schema_version": 1,
            "text_basis": {"correction": {"correction_id": "corr-1", "extra_field": "bad"}},
            "request_version": "req-1",
            "item_type": "Action",
            "reminder_proposal": null,
            "session_topic_proposal": null,
            "source_spans": [{"start": 0, "end": 5}],
            "abstention": null
        }"#;

        let result: serde_json::Result<Proposal> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // === Negative Revision Validation ===

    #[test]
    fn test_proposal_negative_source_revision_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            -5,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("non-negative"));
    }

    // === Empty Correction ID Validation ===

    #[test]
    fn test_empty_correction_id_rejected() {
        let text = "Some text";
        let basis = TextBasis::Correction {
            correction_id: "".to_string(),
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            basis,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("correction_id"));
    }

    // === Type-Only Proposal Must Have Source Spans ===

    #[test]
    fn test_type_only_proposal_without_source_spans_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("source span"));
    }

    #[test]
    fn test_type_only_proposal_with_empty_source_spans_rejected() {
        let text = "Some text";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Action))
        .with_source_spans(Some(vec![]));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("source span"));
    }

    // === Reminder Must Have Source Span ===

    #[test]
    fn test_reminder_without_source_span_rejected() {
        let text = "Some text";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: None,
        };

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_reminder_proposal(Some(reminder));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("source evidence"));
    }

    // === Session Topic With Null Source Span ===

    #[test]
    fn test_session_topic_with_null_source_span_rejected() {
        let text = "meeting notes";
        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "work".to_string(),
            source_span: None,
        }));

        let result = proposal.validate(text);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("source evidence"));
    }
}
