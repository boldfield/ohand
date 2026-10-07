/// Smoke test: verify basic workspace structure and serialization support.
///
/// This test confirms that the core workspace can compile, that key dependencies
/// are available, that serialization contracts work as expected, and that the
/// core crate exports a meaningful public interface.

#[test]
fn test_smoke_uuid_serialization() {
    use serde_json;
    use uuid::Uuid;

    let id = Uuid::new_v4();
    let serialized = serde_json::to_string(&id).expect("UUID should serialize");
    let deserialized: Uuid = serde_json::from_str(&serialized).expect("UUID should deserialize");

    assert_eq!(id, deserialized, "UUID round-trip should preserve value");
}

#[test]
fn test_smoke_chrono_serialization() {
    use chrono::{DateTime, Utc};
    use serde_json;

    let now: DateTime<Utc> = Utc::now();
    let serialized = serde_json::to_string(&now).expect("DateTime should serialize");
    let deserialized: DateTime<Utc> =
        serde_json::from_str(&serialized).expect("DateTime should deserialize");

    assert_eq!(
        now, deserialized,
        "DateTime round-trip should preserve value"
    );
}

#[test]
fn test_smoke_serde_json() {
    use serde::{Deserialize, Serialize};
    use serde_json;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestRecord {
        id: String,
        count: u32,
    }

    let record = TestRecord {
        id: "test-123".to_string(),
        count: 42,
    };

    let serialized = serde_json::to_string(&record).expect("Record should serialize");
    let deserialized: TestRecord =
        serde_json::from_str(&serialized).expect("Record should deserialize");

    assert_eq!(
        record, deserialized,
        "Record round-trip should preserve value"
    );
}

#[test]
fn test_smoke_core_interface() {
    use ohand_core::{domain, interpretation, store, CORE_VERSION};

    // Verify that the core crate exports a meaningful public interface.
    // This test confirms the version contract and module accessibility.
    assert!(!CORE_VERSION.is_empty(), "CORE_VERSION should be non-empty");
    assert_eq!(
        CORE_VERSION, "0.1.0-m1",
        "CORE_VERSION should match the M1 contract"
    );

    // Verify that core modules are importable. As downstream feature tasks
    // fill F02-owned modules, they will exercise concrete types in these paths.
    let _ = (domain, interpretation, store);
}
