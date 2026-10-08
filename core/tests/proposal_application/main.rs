mod proposal_application_tests {
    use ohand_core::domain::items::{
        FieldProvenance, ItemState, LifecycleState, TextState, SUPPORTED_PROPOSAL_SCHEMA_VERSION,
    };
    use ohand_core::interpretation::apply::{apply_proposal, ApplyError, ApplyOutcome};
    use ohand_core::interpretation::contracts::{
        AbstentionReason, Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
    };
    use ohand_core::providers::contracts::TextBasis;
    use ohand_core::store::events::{ItemScope, ItemType};
    use rusqlite::Connection;

    const PROPOSAL_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
    const ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
    const CAPTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
    const REQUEST_VERSION: &str = "550e8400-e29b-41d4-a716-446655440004";
    const TEXT: &str = "Remind me tomorrow at 9am";

    fn base_proposal() -> Proposal {
        Proposal::new(
            PROPOSAL_ID.to_string(),
            ITEM_ID.to_string(),
            CAPTURE_ID.to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original { item_revision: 0 },
            REQUEST_VERSION.to_string(),
        )
    }

    fn base_item() -> ItemState {
        ItemState {
            item_id: ITEM_ID.to_string(),
            capture_id: CAPTURE_ID.to_string(),
            revision: 0,
            item_type: None,
            scope: ItemScope::Personal,
            session_topic: None,
            lifecycle_state: LifecycleState::Active,
            current_text: TextState::Original {
                text: Some(TEXT.to_string()),
            },
            provenance: FieldProvenance::default(),
        }
    }

    #[test]
    fn test_valid_proposal_applied_successfully() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let item = base_item();
        let proposal = base_proposal();

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ApplyOutcome::Applied);
    }

    #[test]
    fn test_deleted_item_cannot_be_modified() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.lifecycle_state = LifecycleState::Deleted;
        let proposal = base_proposal();

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::DeletedItem { item_id }) => {
                assert_eq!(item_id, ITEM_ID);
            }
            other => panic!("Expected DeletedItem error, got {:?}", other),
        }
    }

    #[test]
    fn test_stale_revision_rejected() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.revision = 5;
        let proposal = base_proposal();

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::StaleRevision { expected, current }) => {
                assert_eq!(expected, 0);
                assert_eq!(current, 5);
            }
            other => panic!("Expected StaleRevision error, got {:?}", other),
        }
    }

    #[test]
    fn test_capture_id_mismatch_rejected() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.capture_id = "550e8400-e29b-41d4-a716-446655440099".to_string();
        let proposal = base_proposal();

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::CaptureIdMismatch) => {}
            other => panic!("Expected CaptureIdMismatch error, got {:?}", other),
        }
    }

    #[test]
    fn test_abstention_is_accepted_and_applied() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let item = base_item();
        let proposal = base_proposal().with_abstention(Some(AbstentionReason::Ambiguous));

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ApplyOutcome::Applied);
    }

    #[test]
    fn test_abstention_for_unsupported_operation() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let item = base_item();
        let proposal = base_proposal()
            .with_abstention(Some(AbstentionReason::UnsupportedOperation))
            .with_item_type(None);

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reminder_requires_action_type() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let item = base_item();
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: Some("2026-10-09T09:00:00Z".to_string()),
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::ReminderRequiresActionType) => {}
            other => panic!("Expected ReminderRequiresActionType error, got {:?}", other),
        }
    }

    #[test]
    fn test_reminder_accepted_when_item_type_is_action() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: Some("2026-10-09T09:00:00Z".to_string()),
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reminder_accepted_when_proposal_sets_action_type() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let item = base_item();
        let proposal = base_proposal()
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![SourceSpan::new(0, 6)]))
            .with_reminder_proposal(Some(ReminderProposal {
                instant: Some("2026-10-09T09:00:00Z".to_string()),
                timezone_id: Some("UTC".to_string()),
                quality: TimeResolutionQuality::Explicit,
                source_span: Some(SourceSpan::new(0, 6)),
            }));

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ambiguous_reminder_without_instant() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: None,
            timezone_id: None,
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ambiguous_reminder_with_instant_rejected() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: Some("2026-10-09T09:00:00Z".to_string()),
            timezone_id: None,
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::AmbiguousReminderWithInstant) => {}
            other => panic!(
                "Expected AmbiguousReminderWithInstant error, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_explicit_reminder_requires_instant() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: None,
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::MissingReminderInstant) => {}
            other => panic!("Expected MissingReminderInstant error, got {:?}", other),
        }
    }

    #[test]
    fn test_inferred_reminder_requires_instant() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: None,
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Inferred,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::MissingReminderInstant) => {}
            other => panic!("Expected MissingReminderInstant error, got {:?}", other),
        }
    }

    #[test]
    fn test_non_action_item_types_cannot_carry_reminders() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Note);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: Some("2026-10-09T09:00:00Z".to_string()),
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::ReminderRequiresActionType) => {}
            other => panic!("Expected ReminderRequiresActionType error, got {:?}", other),
        }
    }

    #[test]
    fn test_invalid_reminder_instant_rejected() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.item_type = Some(ItemType::Action);
        let proposal = base_proposal().with_reminder_proposal(Some(ReminderProposal {
            instant: Some("not-a-valid-rfc3339-instant".to_string()),
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(0, 6)),
        }));

        let result = apply_proposal(&tx, &item, &proposal);
        match result {
            Err(ApplyError::MissingReminderInstant) => {}
            other => panic!("Expected MissingReminderInstant error, got {:?}", other),
        }
    }

    #[test]
    fn test_corrected_text_basis_recognized() {
        let mut conn = Connection::open_in_memory().expect("failed to open in-memory database");
        let tx = conn.transaction().expect("failed to open transaction");
        let mut item = base_item();
        item.current_text = TextState::Corrected {
            text: TEXT.to_string(),
            corrected_at: "2026-10-08T10:00:00Z".to_string(),
        };
        let proposal = Proposal::new(
            PROPOSAL_ID.to_string(),
            ITEM_ID.to_string(),
            CAPTURE_ID.to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Correction {
                correction_record_id: "550e8400-e29b-41d4-a716-446655440005".to_string(),
                item_revision: 0,
            },
            REQUEST_VERSION.to_string(),
        );

        let result = apply_proposal(&tx, &item, &proposal);
        assert!(result.is_ok());
    }
}
