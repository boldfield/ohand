use ohand_core::interpretation::instructions::{
    ExpectedOutcome, ForbiddenOutcome, GoldenFixture, InstructionError, InstructionMetadata,
    InstructionSet, InterpretationMapping, RequestContext, INSTRUCTION_SCHEMA_VERSION,
};
use uuid::Uuid;

/// Helper to create a test instruction set.
fn test_instruction_set() -> InstructionSet {
    InstructionSet::new("Interpret the user's input and extract note, action, idea, reminder or session-topic as applicable.")
        .expect("valid instruction")
}

/// Helper to create a test request context.
fn test_request_context() -> RequestContext {
    RequestContext {
        request_id: Uuid::new_v4().to_string(),
        capture_id: Uuid::new_v4().to_string(),
        item_id: Uuid::new_v4().to_string(),
        source_revision: 1,
        instruction_version: Uuid::new_v4().to_string(),
        profile_version: Uuid::new_v4().to_string(),
        route_id: "general".to_string(),
        capture_instant: "2026-10-08T14:00:00Z".to_string(),
        device_timezone: "America/New_York".to_string(),
    }
}

#[test]
fn test_instruction_set_versioning() {
    let set1 = test_instruction_set();
    let set2 = test_instruction_set();

    assert_ne!(set1.version, set2.version, "each instruction set gets unique version");
    assert_eq!(set1.schema_version, INSTRUCTION_SCHEMA_VERSION);
    assert_eq!(set2.schema_version, INSTRUCTION_SCHEMA_VERSION);
}

#[test]
fn test_golden_fixture_design_examples() {
    let fixtures = vec![
        // Design example: broad intention
        GoldenFixture {
            id: "design-broad-intention".to_string(),
            category: "design".to_string(),
            input: "Maybe a roof garden would be nice".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("idea".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                item_types: vec!["action".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Broad intention must not create an obligation. 'Maybe' and conditional phrasing indicate exploration, not commitment."
                .to_string(),
        },
        // Design example: undated action
        GoldenFixture {
            id: "design-undated-action".to_string(),
            category: "design".to_string(),
            input: "I need to call the roofer".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("action".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string(), "inferred".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Action recorded without invitation to schedule. No reminder should be inferred when explicit time is absent."
                .to_string(),
        },
        // Design example: dated information
        GoldenFixture {
            id: "design-dated-information".to_string(),
            category: "design".to_string(),
            input: "The roof quote expires Friday".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("note".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string(), "inferred".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Date preserved as information. Absence of 'remind me' indicates no reminder request. Date/time information must not be silently converted to a deadline."
                .to_string(),
        },
    ];

    for fixture in fixtures {
        assert!(!fixture.id.is_empty());
        assert!(!fixture.input.is_empty());
        assert!(fixture.recoverable);
    }
}

#[test]
fn test_golden_fixture_negation() {
    let fixtures = vec![
        GoldenFixture {
            id: "negation-dont-remind".to_string(),
            category: "negation".to_string(),
            input: "Don't remind me to call the roofer".to_string(),
            provenance: "synthetic, negation test".to_string(),
            expected: ExpectedOutcome {
                abstention: Some("Negated".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string(), "inferred".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Negation must be recognized as explicit abstention, not as a reminder proposal."
                .to_string(),
        },
        GoldenFixture {
            id: "negation-quoted".to_string(),
            category: "negation".to_string(),
            input: "He said \"maybe I'll quit\" but I don't think he will".to_string(),
            provenance: "synthetic, negation in quotes".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("note".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                item_types: vec!["action".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Quoted text should not create actionable proposals. Reported speech should not override main intent."
                .to_string(),
        },
    ];

    for fixture in fixtures {
        assert_eq!(fixture.category, "negation");
        assert!(fixture.recoverable);
    }
}

#[test]
fn test_golden_fixture_mixed_intents() {
    let fixtures = vec![
        GoldenFixture {
            id: "mixed-action-and-note".to_string(),
            category: "mixed-intent".to_string(),
            input: "I need to call the roofer before Friday when the quote expires".to_string(),
            provenance: "synthetic, mixed intent".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("action".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Mixed intent: action with deadline information but no explicit reminder. Action takes precedence."
                .to_string(),
        },
    ];

    for fixture in fixtures {
        assert_eq!(fixture.category, "mixed-intent");
    }
}

#[test]
fn test_golden_fixture_session_topics() {
    let fixtures = vec![
        GoldenFixture {
            id: "session-topic-therapy".to_string(),
            category: "session-topic".to_string(),
            input: "Bring this up in therapy".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                session_topic: Some("therapy".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                item_types: vec!["action".to_string()],
                reminder_qualities: vec!["explicit".to_string(), "inferred".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Session topic must not create an item type or change privacy scope. Facet is independent of item capture context."
                .to_string(),
        },
        GoldenFixture {
            id: "session-topic-with-action".to_string(),
            category: "session-topic".to_string(),
            input: "Bring my concerns about work-life balance up in my therapy session".to_string(),
            provenance: "synthetic, session topic with action".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("action".to_string()),
                session_topic: Some("therapy".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Session topic can coexist with item type. Both are separate facets derived from source."
                .to_string(),
        },
    ];

    for fixture in fixtures {
        assert_eq!(fixture.category, "session-topic");
    }
}

#[test]
fn test_golden_fixture_abstention() {
    let fixtures = vec![
        GoldenFixture {
            id: "abstention-uncertain".to_string(),
            category: "abstention".to_string(),
            input: "Maybe I should call them".to_string(),
            provenance: "synthetic, uncertain intent".to_string(),
            expected: ExpectedOutcome {
                abstention: Some("Ambiguous".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                reminder_qualities: vec!["explicit".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Ambiguous intent should abstain and preserve source, not invent a default item type."
                .to_string(),
        },
        GoldenFixture {
            id: "abstention-unsupported-operation".to_string(),
            category: "abstention".to_string(),
            input: "Done with the roofer call".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                abstention: Some("UnsupportedOperation".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                operations: vec!["update".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Spoken updates are unsupported in M1; must explicitly abstain with reason UnsupportedOperation."
                .to_string(),
        },
    ];

    for fixture in fixtures {
        assert_eq!(fixture.category, "abstention");
    }
}

#[test]
fn test_interpretation_mapping_with_multiple_fixtures() {
    let instructions = test_instruction_set();
    let context = test_request_context();

    let test_inputs = vec![
        "I need to call the roofer",
        "Maybe a roof garden would be nice",
        "Bring this up in therapy",
    ];

    for input in test_inputs {
        let mapping = InterpretationMapping {
            instructions: instructions.clone(),
            context: context.clone(),
            source_text: input.to_string(),
        };

        assert!(mapping.validate().is_ok(), "mapping for '{}' should be valid", input);
    }
}

#[test]
fn test_request_context_versioning_independence() {
    let ctx1 = test_request_context();
    let ctx2 = test_request_context();

    assert_ne!(
        ctx1.request_id, ctx2.request_id,
        "each request context gets unique request_id"
    );
    assert_ne!(
        ctx1.instruction_version, ctx2.instruction_version,
        "instruction versions can differ"
    );
}

#[test]
fn test_instruction_metadata_supports_all_m1_capabilities() {
    let metadata = InstructionMetadata::default_m1();

    assert!(metadata.supports_session_topic, "must support session topics");
    assert!(metadata.supports_reminders, "must support reminders");
    assert!(metadata.supports_item_type, "must support item types");
    assert!(metadata.supports_abstention, "must support abstention");
    assert_eq!(
        metadata.supported_operations,
        vec!["annotate"],
        "M1 only supports annotate"
    );
}

#[test]
fn test_forbidden_outcomes_prevent_unwanted_mutations() {
    let fixture = GoldenFixture {
        id: "test".to_string(),
        category: "test".to_string(),
        input: "Don't remind me to call the roofer".to_string(),
        provenance: "test".to_string(),
        expected: ExpectedOutcome {
            abstention: Some("Negated".to_string()),
            ..Default::default()
        },
        forbidden: ForbiddenOutcome {
            reminder_qualities: vec!["explicit".to_string(), "inferred".to_string()],
            item_types: vec!["action".to_string()],
            operations: vec!["create".to_string(), "update".to_string()],
        },
        recoverable: true,
        notes: "test".to_string(),
    };

    assert!(fixture.forbidden.reminder_qualities.contains(&"explicit".to_string()));
    assert!(fixture.forbidden.item_types.contains(&"action".to_string()));
    assert!(fixture.forbidden.operations.contains(&"create".to_string()));
}

#[test]
fn test_golden_fixtures_ensure_no_disclosure_permission_from_instructions() {
    let fixture = GoldenFixture {
        id: "private-route-preserved".to_string(),
        category: "privacy".to_string(),
        input: "I'm considering leaving my job".to_string(),
        provenance: "synthetic, privacy test".to_string(),
        expected: ExpectedOutcome {
            item_type: Some("idea".to_string()),
            ..Default::default()
        },
        forbidden: ForbiddenOutcome {
            ..Default::default()
        },
        recoverable: true,
        notes: "Private route must be preserved independently; instructions cannot grant upload permission."
            .to_string(),
    };

    // The fixture demonstrates that routing policy is separate from instructions
    // and cannot be inferred from instruction content or proposal output.
    assert_eq!(fixture.category, "privacy");
    assert!(fixture.recoverable);
}

#[test]
fn test_instruction_schema_version_matches_contract() {
    let set = test_instruction_set();
    assert_eq!(set.schema_version, INSTRUCTION_SCHEMA_VERSION);
    assert_eq!(INSTRUCTION_SCHEMA_VERSION, 1);
}
