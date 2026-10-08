mod proposal_tests {
    use ohand_core::interpretation::contracts::{
        Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
        SUPPORTED_PROPOSAL_SCHEMA_VERSION,
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
        assert!(span.is_valid(text));
    }

    #[test]
    fn test_source_span_out_of_bounds() {
        let text = "Hello";
        let span = SourceSpan::new(0, 10);
        assert!(!span.is_valid(text));
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
        assert!(!proposal.abstained);
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
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some((22, 29)),
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

        let result = proposal.validate_reminder_proposal();
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_full_valid_with_all_fields() {
        let text = "Call the roofer on Friday";
        let reminder = ReminderProposal {
            instant: "2026-10-10T15:00:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some((19, 25)), // "Friday"
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
        .with_session_topic_proposal(Some("work".to_string()))
        .with_source_spans(Some(spans));

        let result = proposal.validate(text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_proposal_validate_type_only() {
        let text = "Maybe a roof garden would be nice";

        let proposal = Proposal::new(
            "prop-1".to_string(),
            "item-1".to_string(),
            "cap-1".to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original,
            "req-1".to_string(),
        )
        .with_item_type(Some(ItemType::Idea));

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
        .with_abstention(true);

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
            source_span: Some((22, 28)),
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
        .with_abstention(true);

        let result = proposal.validate(text);
        assert!(result.is_ok());
        assert!(proposal.abstained);
    }
}
