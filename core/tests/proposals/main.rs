mod proposal_tests {
    use chrono::{TimeZone, Utc};
    use ohand_core::domain::items::{
        FieldProvenance, ItemState, LifecycleState, TextState, SUPPORTED_PROPOSAL_SCHEMA_VERSION,
    };
    use ohand_core::interpretation::contracts::{
        resolve_session_topic, AbstentionReason, Operation, Proposal, ProposalError,
        ReminderProposal, ResolvedSessionTopic, SessionTopicProposal, SourceSpan,
        TimeResolutionQuality,
    };
    use ohand_core::providers::contracts::{
        CapabilityMetadata, InterpretationOutput, InterpretationRequest, ProviderCapability,
        ProviderProfile, ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode, TextBasis,
    };
    use ohand_core::store::events::{ItemScope, ItemType};
    use ohand_core::time::TimeContext;
    use serde_json::{json, Value};

    const PROPOSAL_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
    const ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
    const CAPTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
    const REQUEST_VERSION: &str = "550e8400-e29b-41d4-a716-446655440004";
    const OTHER_ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440005";
    const TEXT: &str = "Call the dentist tomorrow at 9am about therapy";

    fn base() -> Proposal {
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

    fn profile() -> ProviderProfile {
        ProviderProfileBuilder::new(
            "synthetic-self-hosted",
            ProviderProtocol::SelfHosted,
            "model-a",
        )
        .endpoint("https://llm.example.test:8443/v1/interpret")
        .credential_ref("cred-ref-1")
        .timeout_seconds(30)
        .authorized_destination("https://llm.example.test:8443")
        .capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "adapter-test:fake",
            )
            .with_input_size_limit(200)
            .with_structured_output(StructuredOutputMode::JsonObject),
        )
        .build()
        .expect("valid profile")
    }

    fn request_with(text_basis: TextBasis, source_revision: u64) -> InterpretationRequest {
        InterpretationRequest::new(
            CAPTURE_ID,
            source_revision,
            text_basis,
            TEXT,
            REQUEST_VERSION,
            "instructions-v1",
            &profile(),
            "route-secret-name",
            TimeContext {
                timezone: "UTC".to_string(),
                locale: "en-US".to_string(),
                reference_time: Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
                utc_offset_at_capture: 0,
                calendar: "gregorian".to_string(),
            },
        )
        .expect("valid request")
    }

    fn request() -> InterpretationRequest {
        request_with(TextBasis::Original { item_revision: 0 }, 0)
    }

    fn output_for(value: &Value, request_version: &str) -> InterpretationOutput {
        InterpretationOutput {
            request_version: request_version.to_string(),
            proposal: value.as_object().cloned().unwrap_or_default(),
            elapsed_ms: 5,
        }
    }

    fn item_state(session_topic: Option<&str>, topic_corrected: bool) -> ItemState {
        ItemState {
            item_id: ITEM_ID.to_string(),
            capture_id: CAPTURE_ID.to_string(),
            revision: 0,
            item_type: None,
            scope: ItemScope::Personal,
            session_topic: session_topic.map(str::to_string),
            lifecycle_state: LifecycleState::Active,
            current_text: TextState::Original {
                text: Some(TEXT.to_string()),
            },
            provenance: FieldProvenance {
                session_topic_corrected: topic_corrected,
                ..FieldProvenance::default()
            },
        }
    }

    fn proposal_with_topic() -> Proposal {
        base().with_session_topic_proposal(Some(topic()))
    }

    fn typed() -> Proposal {
        base()
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![SourceSpan::new(0, 4)]))
    }

    fn explicit_reminder() -> ReminderProposal {
        ReminderProposal {
            instant: Some("2026-10-09T09:00:00Z".to_string()),
            timezone_id: Some("America/New_York".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(24, 34)),
        }
    }

    fn ambiguous_reminder() -> ReminderProposal {
        ReminderProposal {
            instant: None,
            timezone_id: None,
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(24, 32)),
        }
    }

    fn topic() -> SessionTopicProposal {
        SessionTopicProposal {
            topic: "therapy".to_string(),
            source_span: Some(SourceSpan::new(39, 46)),
        }
    }

    fn valid_json() -> Value {
        json!({
            "proposal_id": PROPOSAL_ID,
            "item_id": ITEM_ID,
            "capture_id": CAPTURE_ID,
            "source_revision": 0,
            "schema_version": SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            "text_basis": {"kind": "original", "item_revision": 0},
            "request_version": REQUEST_VERSION,
            "operation": {"kind": "annotate"},
            "item_type": "action",
            "reminder_proposal": {
                "instant": "2026-10-09T09:00:00Z",
                "timezone_id": "America/New_York",
                "quality": "explicit",
                "source_span": {"start": 24, "end": 34}
            },
            "session_topic_proposal": {
                "topic": "therapy",
                "source_span": {"start": 39, "end": 46}
            },
            "source_spans": [{"start": 0, "end": 4}],
            "abstention": null
        })
    }

    fn parse(value: &Value) -> Result<Proposal, ProposalError> {
        Proposal::from_output(&request(), &output_for(value, REQUEST_VERSION), ITEM_ID)
    }

    // --- Happy paths and provenance ---

    #[test]
    fn full_proposal_round_trips_and_validates() {
        let proposal = parse(&valid_json()).expect("valid proposal");
        assert_eq!(proposal.operation, Operation::Annotate {});
        assert_eq!(proposal.item_type, Some(ItemType::Action));
        let reserialized = serde_json::to_string(&proposal).unwrap();
        let reserialized: Value = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(parse(&reserialized).unwrap(), proposal);
    }

    #[test]
    fn typed_reminder_and_topic_each_validate() {
        assert_eq!(typed().validate(TEXT), Ok(()));
        let reminder = base().with_reminder_proposal(Some(explicit_reminder()));
        assert_eq!(reminder.validate(TEXT), Ok(()));
        let topic_only = base().with_session_topic_proposal(Some(topic()));
        assert_eq!(topic_only.validate(TEXT), Ok(()));
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let mut proposal = typed();
        proposal.schema_version = SUPPORTED_PROPOSAL_SCHEMA_VERSION + 1;
        assert!(matches!(
            proposal.validate(TEXT),
            Err(ProposalError::UnsupportedSchemaVersion { .. })
        ));
    }

    #[test]
    fn correction_basis_keeps_revision_and_requires_uuid_and_matching_revision() {
        let basis = TextBasis::Correction {
            correction_record_id: OTHER_ITEM_ID.to_string(),
            item_revision: 2,
        };
        let mut proposal = typed();
        proposal.source_revision = 2;
        proposal.text_basis = basis.clone();
        assert_eq!(proposal.validate(TEXT), Ok(()));
        let encoded = serde_json::to_value(&proposal.text_basis).unwrap();
        assert_eq!(
            encoded,
            json!({"kind": "correction", "correction_record_id": OTHER_ITEM_ID, "item_revision": 2})
        );

        for bad in ["", "corr-123"] {
            proposal.text_basis = TextBasis::Correction {
                correction_record_id: bad.to_string(),
                item_revision: 2,
            };
            assert_eq!(
                proposal.validate(TEXT),
                Err(ProposalError::InvalidIdentifier {
                    field: "correction_record_id"
                })
            );
        }
        proposal.text_basis = basis;
        proposal.source_revision = 3;
        assert_eq!(
            proposal.validate(TEXT),
            Err(ProposalError::BasisRevisionMismatch {
                basis_revision: 2,
                source_revision: 3
            })
        );
        let mut original = typed();
        original.source_revision = 1;
        assert_eq!(
            original.validate(TEXT),
            Err(ProposalError::BasisRevisionMismatch {
                basis_revision: 0,
                source_revision: 1
            })
        );
    }

    // --- Identifiers and revisions ---

    #[test]
    fn empty_and_non_uuid_identifiers_are_rejected() {
        type Mutator = fn(&mut Proposal, String);
        let fields: [(&'static str, Mutator); 4] = [
            ("proposal_id", |p, v| p.proposal_id = v),
            ("item_id", |p, v| p.item_id = v),
            ("capture_id", |p, v| p.capture_id = v),
            ("request_version", |p, v| p.request_version = v),
        ];
        for (field, set) in fields {
            for bad in ["", "prop-1", "not-a-uuid"] {
                let mut proposal = typed();
                set(&mut proposal, bad.to_string());
                assert_eq!(
                    proposal.validate(TEXT),
                    Err(ProposalError::InvalidIdentifier { field }),
                    "{field} = {bad:?}"
                );
            }
        }
    }

    #[test]
    fn negative_source_revision_is_rejected() {
        let mut proposal = typed();
        proposal.source_revision = -5;
        assert_eq!(
            proposal.validate(TEXT),
            Err(ProposalError::NegativeRevision(-5))
        );
    }

    // --- Spans: characters, bounds, no panics ---

    #[test]
    fn span_at_end_of_text_is_rejected_without_panicking() {
        assert_eq!(
            SourceSpan::new(3, 3).is_valid("abc"),
            Err(ProposalError::EmptySpan { start: 3, end: 3 })
        );
        assert_eq!(
            SourceSpan::new(5, 3).is_valid("abc"),
            Err(ProposalError::EmptySpan { start: 5, end: 3 })
        );
        assert_eq!(
            SourceSpan::new(0, 9999).is_valid("abc"),
            Err(ProposalError::SpanOutOfBounds {
                start: 0,
                end: 9999,
                char_count: 3
            })
        );
        assert_eq!(SourceSpan::new(0, 3).is_valid("abc"), Ok(()));
    }

    #[test]
    fn spans_count_unicode_scalars_not_bytes() {
        let text = "h\u{e9}llo";
        assert_eq!(text.len(), 6);
        assert_eq!(SourceSpan::new(0, 5).is_valid(text), Ok(()));
        assert!(matches!(
            SourceSpan::new(0, 6).is_valid(text),
            Err(ProposalError::SpanOutOfBounds { char_count: 5, .. })
        ));
        assert_eq!(SourceSpan::new(1, 2).is_valid(text), Ok(()));
    }

    #[test]
    fn out_of_bounds_spans_are_rejected_on_every_facet() {
        let oob = Some(SourceSpan::new(0, 9999));
        let type_spans = typed().with_source_spans(Some(vec![SourceSpan::new(0, 9999)]));
        let mut reminder = explicit_reminder();
        reminder.source_span = oob;
        let reminder = base().with_reminder_proposal(Some(reminder));
        let topic_proposal = base().with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "therapy".to_string(),
            source_span: oob,
        }));
        for proposal in [type_spans, reminder, topic_proposal] {
            assert!(matches!(
                proposal.validate(TEXT),
                Err(ProposalError::SpanOutOfBounds { .. })
            ));
        }
    }

    // --- Evidence per facet ---

    #[test]
    fn item_type_requires_non_empty_evidence_even_with_other_facets() {
        for spans in [None, Some(vec![])] {
            let alone = base()
                .with_item_type(Some(ItemType::Note))
                .with_source_spans(spans.clone());
            let with_topic = alone.clone().with_session_topic_proposal(Some(topic()));
            let with_reminder = alone
                .clone()
                .with_reminder_proposal(Some(explicit_reminder()));
            for proposal in [alone, with_topic, with_reminder] {
                assert_eq!(
                    proposal.validate(TEXT),
                    Err(ProposalError::MissingEvidence { facet: "item type" })
                );
            }
        }
    }

    #[test]
    fn reminder_and_topic_require_their_own_evidence() {
        let mut reminder = explicit_reminder();
        reminder.source_span = None;
        assert_eq!(
            typed()
                .with_reminder_proposal(Some(reminder))
                .validate(TEXT),
            Err(ProposalError::MissingEvidence { facet: "reminder" })
        );
        let mut unsourced_topic = topic();
        unsourced_topic.source_span = None;
        assert_eq!(
            typed()
                .with_session_topic_proposal(Some(unsourced_topic))
                .validate(TEXT),
            Err(ProposalError::MissingEvidence {
                facet: "session topic"
            })
        );
        let mut ambiguous = ambiguous_reminder();
        ambiguous.source_span = None;
        assert_eq!(
            typed()
                .with_reminder_proposal(Some(ambiguous))
                .validate(TEXT),
            Err(ProposalError::MissingEvidence { facet: "reminder" })
        );
    }

    #[test]
    fn empty_evidence_span_and_empty_topic_are_rejected() {
        let mut reminder = explicit_reminder();
        reminder.source_span = Some(SourceSpan::new(3, 3));
        assert!(matches!(
            typed()
                .with_reminder_proposal(Some(reminder))
                .validate(TEXT),
            Err(ProposalError::EmptySpan { .. })
        ));
        let blank = SessionTopicProposal {
            topic: "   ".to_string(),
            source_span: Some(SourceSpan::new(0, 4)),
        };
        assert_eq!(
            typed()
                .with_session_topic_proposal(Some(blank))
                .validate(TEXT),
            Err(ProposalError::EmptyField {
                field: "session topic"
            })
        );
    }

    // --- Reminder time: explicit, ambiguous, invalid ---

    #[test]
    fn invalid_instant_and_timezone_are_rejected() {
        let mut reminder = explicit_reminder();
        reminder.instant = Some("not a time".to_string());
        assert_eq!(
            typed()
                .with_reminder_proposal(Some(reminder))
                .validate(TEXT),
            Err(ProposalError::InvalidInstant)
        );
        let mut reminder = explicit_reminder();
        reminder.timezone_id = Some("Nowhere/Fake".to_string());
        assert_eq!(
            typed()
                .with_reminder_proposal(Some(reminder))
                .validate(TEXT),
            Err(ProposalError::InvalidTimezone)
        );
    }

    #[test]
    fn explicit_and_inferred_times_require_instant_and_timezone() {
        for quality in [
            TimeResolutionQuality::Explicit,
            TimeResolutionQuality::Inferred,
        ] {
            let mut missing_instant = explicit_reminder();
            missing_instant.quality = quality;
            missing_instant.instant = None;
            assert_eq!(
                typed()
                    .with_reminder_proposal(Some(missing_instant))
                    .validate(TEXT),
                Err(ProposalError::MissingReminderResolution { field: "instant" })
            );
            let mut missing_zone = explicit_reminder();
            missing_zone.quality = quality;
            missing_zone.timezone_id = None;
            assert_eq!(
                typed()
                    .with_reminder_proposal(Some(missing_zone))
                    .validate(TEXT),
                Err(ProposalError::MissingReminderResolution { field: "timezone" })
            );
        }
    }

    #[test]
    fn ambiguous_time_is_represented_without_a_guessed_instant() {
        let proposal = typed().with_reminder_proposal(Some(ambiguous_reminder()));
        assert_eq!(proposal.validate(TEXT), Ok(()));

        let mut guessed = ambiguous_reminder();
        guessed.instant = Some("2026-10-09T09:00:00Z".to_string());
        assert_eq!(
            typed().with_reminder_proposal(Some(guessed)).validate(TEXT),
            Err(ProposalError::AmbiguousReminderHasInstant)
        );

        let mut bad_zone = ambiguous_reminder();
        bad_zone.timezone_id = Some("Nowhere/Fake".to_string());
        assert_eq!(
            typed()
                .with_reminder_proposal(Some(bad_zone))
                .validate(TEXT),
            Err(ProposalError::InvalidTimezone)
        );
    }

    #[test]
    fn time_resolution_quality_parses_and_displays() {
        for quality in [
            TimeResolutionQuality::Explicit,
            TimeResolutionQuality::Inferred,
            TimeResolutionQuality::Ambiguous,
        ] {
            assert_eq!(
                quality.as_str().parse::<TimeResolutionQuality>(),
                Ok(quality)
            );
        }
        assert!("vague".parse::<TimeResolutionQuality>().is_err());
    }

    // --- Abstention ---

    #[test]
    fn abstention_alone_validates_for_every_reason() {
        for reason in [
            AbstentionReason::UncertainTarget,
            AbstentionReason::Negated,
            AbstentionReason::Ambiguous,
            AbstentionReason::UnsupportedOperation,
            AbstentionReason::Other("model refused".to_string()),
        ] {
            assert_eq!(base().with_abstention(Some(reason)).validate(TEXT), Ok(()));
        }
    }

    #[test]
    fn abstention_excludes_every_facet() {
        let abstain = || base().with_abstention(Some(AbstentionReason::Negated));
        let cases = [
            (abstain().with_item_type(Some(ItemType::Note)), "item type"),
            (
                abstain().with_reminder_proposal(Some(explicit_reminder())),
                "reminder",
            ),
            (
                abstain().with_session_topic_proposal(Some(topic())),
                "session topic",
            ),
        ];
        for (proposal, facet) in cases {
            assert_eq!(
                proposal.validate(TEXT),
                Err(ProposalError::AbstentionWithContent { facet })
            );
        }
    }

    #[test]
    fn blank_other_abstention_reason_is_rejected() {
        let proposal = base().with_abstention(Some(AbstentionReason::Other(" ".to_string())));
        assert!(matches!(
            proposal.validate(TEXT),
            Err(ProposalError::EmptyField { .. })
        ));
    }

    #[test]
    fn proposal_with_nothing_and_no_abstention_is_rejected() {
        assert_eq!(base().validate(TEXT), Err(ProposalError::NoContent));
    }

    #[test]
    fn uncertain_target_is_an_explicit_abstention_not_a_guess() {
        let mut json = valid_json();
        json["item_type"] = Value::Null;
        json["reminder_proposal"] = Value::Null;
        json["session_topic_proposal"] = Value::Null;
        json["source_spans"] = Value::Null;
        json["abstention"] = json!("UncertainTarget");
        let proposal = parse(&json).expect("explicit abstention");
        assert_eq!(proposal.abstention, Some(AbstentionReason::UncertainTarget));
    }

    #[test]
    fn negated_text_is_only_accepted_as_abstention() {
        // "don't remind me" must not become a reminder: a negated source with facets is rejected.
        let mut json = valid_json();
        json["abstention"] = json!("Negated");
        assert!(matches!(
            parse(&json),
            Err(ProposalError::AbstentionWithContent { .. })
        ));
    }

    // --- Operations: only annotation is supported ---

    #[test]
    fn annotate_is_the_only_supported_operation() {
        assert_eq!(
            typed()
                .with_operation(Operation::Annotate {})
                .validate(TEXT),
            Ok(())
        );
    }

    #[test]
    fn unsupported_operations_must_abstain_as_unsupported() {
        let operations = [
            Operation::Create {},
            Operation::Update {
                item_id: ITEM_ID.to_string(),
            },
        ];
        for operation in operations {
            let name = if matches!(operation, Operation::Create {}) {
                "create"
            } else {
                "update"
            };
            // A spoken change that carries facets is not an abstention.
            assert_eq!(
                typed().with_operation(operation.clone()).validate(TEXT),
                Err(ProposalError::UnsupportedOperationNotAbstained { operation: name })
            );
            // Without any abstention it is rejected too.
            assert_eq!(
                base().with_operation(operation.clone()).validate(TEXT),
                Err(ProposalError::UnsupportedOperationNotAbstained { operation: name })
            );
            // Abstaining with another reason hides the unsupported operation.
            assert_eq!(
                base()
                    .with_operation(operation.clone())
                    .with_abstention(Some(AbstentionReason::Negated))
                    .validate(TEXT),
                Err(ProposalError::UnsupportedOperationWrongReason { operation: name })
            );
            // The only valid representation is the explicit unsupported-operation abstention.
            assert_eq!(
                base()
                    .with_operation(operation.clone())
                    .with_abstention(Some(AbstentionReason::UnsupportedOperation))
                    .validate(TEXT),
                Ok(())
            );
            // And it still cannot carry any facet.
            assert!(matches!(
                typed()
                    .with_operation(operation)
                    .with_abstention(Some(AbstentionReason::UnsupportedOperation))
                    .validate(TEXT),
                Err(ProposalError::AbstentionWithContent { .. })
            ));
        }
    }

    #[test]
    fn update_target_must_be_a_uuid_naming_the_proposal_item() {
        let update = |item_id: &str| {
            base()
                .with_operation(Operation::Update {
                    item_id: item_id.to_string(),
                })
                .with_abstention(Some(AbstentionReason::UnsupportedOperation))
        };
        for bad in ["", "not-a-uuid"] {
            assert_eq!(
                update(bad).validate(TEXT),
                Err(ProposalError::InvalidIdentifier {
                    field: "update item_id"
                })
            );
        }
        assert_eq!(
            update(OTHER_ITEM_ID).validate(TEXT),
            Err(ProposalError::UpdateTargetMismatch)
        );
        assert_eq!(update(ITEM_ID).validate(TEXT), Ok(()));
    }

    #[test]
    fn missing_or_unknown_operation_is_rejected_when_decoding() {
        let mut json = valid_json();
        json.as_object_mut().unwrap().remove("operation");
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));

        let mut json = valid_json();
        json["operation"] = json!({"kind": "delete"});
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));

        let mut json = valid_json();
        json["operation"] = json!({"kind": "annotate", "scope": "work"});
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
    }

    #[test]
    fn update_operation_decodes_and_is_enforced() {
        let mut json = valid_json();
        json["operation"] = json!({"kind": "update", "item_id": ITEM_ID});
        assert_eq!(
            parse(&json),
            Err(ProposalError::UnsupportedOperationNotAbstained {
                operation: "update"
            })
        );
    }

    // --- Decoding untrusted output: unknown fields at every level ---

    #[test]
    fn unknown_fields_are_rejected_at_every_nesting_level() {
        let mutations: [fn(&mut Value); 7] = [
            |v| v["evil_extra"] = json!(true),
            |v| v["text_basis"]["bogus"] = json!(1),
            |v| v["operation"]["bogus"] = json!(1),
            |v| v["reminder_proposal"]["evil_extra"] = json!(true),
            |v| v["reminder_proposal"]["source_span"]["bogus"] = json!(1),
            |v| v["session_topic_proposal"]["bogus"] = json!(1),
            |v| v["source_spans"][0]["bogus"] = json!(1),
        ];
        for mutate in mutations {
            let mut json = valid_json();
            mutate(&mut json);
            assert!(
                matches!(parse(&json), Err(ProposalError::Malformed(_))),
                "{json}"
            );
        }
        let mut json = valid_json();
        json["text_basis"] = json!({
            "kind": "correction",
            "correction_record_id": OTHER_ITEM_ID,
            "item_revision": 0,
            "x": 1
        });
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
    }

    #[test]
    fn privacy_and_authorization_fields_cannot_be_smuggled_in() {
        let top_level = [
            "item_scope",
            "scope",
            "read_scope",
            "session_read_scope",
            "route_id",
            "upload_permission",
            "preview_eligible",
            "authorization",
        ];
        for field in top_level {
            let mut json = valid_json();
            json[field] = json!("work");
            assert!(
                matches!(parse(&json), Err(ProposalError::Malformed(_))),
                "{field}"
            );
        }
        let mut json = valid_json();
        json["session_topic_proposal"]["read_scope"] = json!("private");
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
        let mut json = valid_json();
        json["session_topic_proposal"]["item_scope"] = json!("work");
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
    }

    #[test]
    fn invalid_json_shapes_are_rejected_not_panicked() {
        for shape in [json!({}), json!({"proposal_id": 1}), Value::Null] {
            assert!(matches!(parse(&shape), Err(ProposalError::Malformed(_))));
        }
        let mut json = valid_json();
        json["source_spans"] = json!([{"start": -1, "end": 2}]);
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
        let mut json = valid_json();
        json["item_type"] = json!("task");
        assert!(matches!(parse(&json), Err(ProposalError::Malformed(_))));
    }

    #[test]
    fn decoded_proposals_are_semantically_validated() {
        let mut json = valid_json();
        json["reminder_proposal"]["instant"] = json!("not a time");
        assert_eq!(parse(&json), Err(ProposalError::InvalidInstant));
        let mut json = valid_json();
        json["proposal_id"] = json!("prop-1");
        assert_eq!(
            parse(&json),
            Err(ProposalError::InvalidIdentifier {
                field: "proposal_id"
            })
        );
    }

    // --- Session-topic: independent facet, user correction wins ---

    #[test]
    fn session_topic_is_independent_of_item_type_and_scope() {
        let topic_only = base().with_session_topic_proposal(Some(topic()));
        assert_eq!(topic_only.item_type, None);
        assert_eq!(topic_only.validate(TEXT), Ok(()));
        let serialized = serde_json::to_value(&topic_only).unwrap();
        let object = serialized.as_object().unwrap();
        assert!(object.keys().all(|key| !key.contains("scope")
            && !key.contains("route")
            && !key.contains("permission")));
    }

    #[test]
    fn user_correction_wins_over_capture_topic_and_proposal() {
        let proposal = proposal_with_topic();
        let corrected = item_state(Some("  grief group "), true);
        assert_eq!(
            resolve_session_topic(&corrected, Some("stated"), &proposal, TEXT),
            Ok(Some(ResolvedSessionTopic::UserCorrected {
                topic: "grief group".to_string()
            }))
        );
        // A user who cleared the topic is not overridden by a model proposal.
        assert_eq!(
            resolve_session_topic(&item_state(None, true), None, &proposal, TEXT),
            Ok(None)
        );
    }

    #[test]
    fn topic_stated_at_capture_outranks_a_proposal() {
        let state = item_state(Some("stated"), false);
        assert_eq!(
            resolve_session_topic(&state, Some(" stated "), &proposal_with_topic(), TEXT),
            Ok(Some(ResolvedSessionTopic::StatedAtCapture {
                topic: "stated".to_string()
            }))
        );
    }

    #[test]
    fn proposal_topic_is_derived_only_without_user_input() {
        let state = item_state(None, false);
        assert_eq!(
            resolve_session_topic(&state, None, &proposal_with_topic(), TEXT),
            Ok(Some(ResolvedSessionTopic::Derived {
                topic: "therapy".to_string(),
                evidence: SourceSpan::new(39, 46)
            }))
        );
        assert_eq!(
            resolve_session_topic(&state, Some("   "), &proposal_with_topic(), TEXT),
            resolve_session_topic(&state, None, &proposal_with_topic(), TEXT)
        );
        assert_eq!(
            resolve_session_topic(&state, None, &typed(), TEXT),
            Ok(None)
        );
    }

    #[test]
    fn session_topic_resolution_rejects_invalid_or_foreign_proposals() {
        let state = item_state(None, false);
        let blank_out_of_bounds = base().with_session_topic_proposal(Some(SessionTopicProposal {
            topic: "   ".to_string(),
            source_span: Some(SourceSpan::new(99, 100)),
        }));
        assert!(resolve_session_topic(&state, None, &blank_out_of_bounds, TEXT).is_err());
        let mut unsourced = topic();
        unsourced.source_span = None;
        assert_eq!(
            resolve_session_topic(
                &state,
                None,
                &base().with_session_topic_proposal(Some(unsourced)),
                TEXT
            ),
            Err(ProposalError::MissingEvidence {
                facet: "session topic"
            })
        );
        // Validation applies even when a higher-precedence topic would win.
        assert!(resolve_session_topic(
            &item_state(Some("kept"), true),
            None,
            &blank_out_of_bounds,
            TEXT
        )
        .is_err());

        let mut other_item = proposal_with_topic();
        other_item.item_id = OTHER_ITEM_ID.to_string();
        assert_eq!(
            resolve_session_topic(&state, None, &other_item, TEXT),
            Err(ProposalError::ProvenanceMismatch { field: "item_id" })
        );
        let mut other_capture = proposal_with_topic();
        other_capture.capture_id = OTHER_ITEM_ID.to_string();
        assert_eq!(
            resolve_session_topic(&state, None, &other_capture, TEXT),
            Err(ProposalError::ProvenanceMismatch {
                field: "capture_id"
            })
        );
    }

    // --- Trusted boundary: provenance must equal the dispatched request ---

    #[test]
    fn output_matching_the_trusted_request_is_accepted() {
        let proposal = parse(&valid_json()).expect("matching provenance");
        assert_eq!(proposal.capture_id, CAPTURE_ID);
    }

    #[test]
    fn provenance_must_match_the_trusted_request() {
        let other_uuid = "550e8400-e29b-41d4-a716-4466554400ff";
        let cases: [(&str, Value); 4] = [
            ("capture_id", json!(other_uuid)),
            ("request_version", json!(other_uuid)),
            ("item_id", json!(OTHER_ITEM_ID)),
            ("source_revision", json!(1)),
        ];
        for (field, value) in cases {
            let mut json = valid_json();
            json[field] = value;
            if field == "source_revision" {
                json["text_basis"]["item_revision"] = json!(1);
            }
            assert_eq!(
                parse(&json),
                Err(ProposalError::ProvenanceMismatch { field }),
                "{field}"
            );
        }
        let mut json = valid_json();
        json["text_basis"] = json!({
            "kind": "correction",
            "correction_record_id": OTHER_ITEM_ID,
            "item_revision": 0
        });
        assert_eq!(
            parse(&json),
            Err(ProposalError::ProvenanceMismatch {
                field: "text_basis"
            })
        );
    }

    #[test]
    fn output_request_version_must_equal_the_request() {
        let other_uuid = "550e8400-e29b-41d4-a716-4466554400ff";
        assert_eq!(
            Proposal::from_output(&request(), &output_for(&valid_json(), other_uuid), ITEM_ID),
            Err(ProposalError::ProvenanceMismatch {
                field: "output request_version"
            })
        );
    }

    #[test]
    fn correction_basis_and_revision_must_match_the_request() {
        let basis = TextBasis::Correction {
            correction_record_id: OTHER_ITEM_ID.to_string(),
            item_revision: 3,
        };
        let request = request_with(basis, 3);
        let mut json = valid_json();
        json["source_revision"] = json!(3);
        json["text_basis"] = json!({
            "kind": "correction",
            "correction_record_id": OTHER_ITEM_ID,
            "item_revision": 3
        });
        let output = output_for(&json, REQUEST_VERSION);
        assert!(Proposal::from_output(&request, &output, ITEM_ID).is_ok());

        // The original basis at the same revision is not the corrected text the model saw.
        json["text_basis"] = json!({"kind": "original", "item_revision": 3});
        let output = output_for(&json, REQUEST_VERSION);
        assert_eq!(
            Proposal::from_output(&request, &output, ITEM_ID),
            Err(ProposalError::ProvenanceMismatch {
                field: "text_basis"
            })
        );
    }

    #[test]
    fn spans_are_checked_against_the_request_text() {
        let mut json = valid_json();
        json["source_spans"] = json!([{"start": 0, "end": 9999}]);
        assert!(matches!(
            parse(&json),
            Err(ProposalError::SpanOutOfBounds { .. })
        ));
    }
}
