import XCTest
import Security
@testable import OhAndServices

/// Behavior of `CredentialService` against a fake Keychain boundary, which can return exact `OSStatus` outcomes.
/// The real Keychain is covered by `CredentialKeychainIntegrationTests`.
final class CredentialServiceTests: XCTestCase {
    private let serviceName = "com.boldfield.ohand.tests.credentials.fake"
    private let syntheticSecret = Data("synthetic-provider-credential-one".utf8)
    private let replacementSecret = Data("synthetic-provider-credential-two".utf8)

    private var keychain = FakeKeychain()
    private var service: CredentialService!

    override func setUp() {
        super.setUp()
        keychain = FakeKeychain()
        service = CredentialService(serviceName: serviceName, keychain: keychain)
    }

    private func key(for reference: String) -> KeychainItemKey {
        KeychainItemKey(service: serviceName, account: reference)
    }

    // MARK: add / update / delete

    func testAddReturnsOpaqueUUIDReferenceAndStoresSecretWithRequiredAccessibility() throws {
        let reference = try service.addCredential(syntheticSecret)

        XCTAssertNotNil(UUID(uuidString: reference))
        XCTAssertFalse(reference.contains("synthetic"))
        let stored = try XCTUnwrap(keychain.items[key(for: reference)])
        XCTAssertEqual(stored.secret, syntheticSecret)
        XCTAssertEqual(stored.accessibility, .afterFirstUnlockThisDeviceOnly)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
    }

    func testEachAddProducesDistinctReference() throws {
        let first = try service.addCredential(syntheticSecret)
        let second = try service.addCredential(syntheticSecret)
        XCTAssertNotEqual(first, second)
    }

    func testUpdateOverwritesSecretBehindSameReference() throws {
        let reference = try service.addCredential(syntheticSecret)
        try service.updateCredential(replacementSecret, reference: reference)

        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)
        XCTAssertEqual(keychain.items.count, 1)
    }

    func testUpdateOfMissingItemRebindsSameReference() throws {
        let reference = UUID().uuidString
        try service.updateCredential(syntheticSecret, reference: reference)

        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
        XCTAssertEqual(keychain.callCount(.insert), 1)
    }

    func testUpdateRetriesReplaceOnceWhenInsertRacesWithAnotherWriter() throws {
        let reference = UUID().uuidString
        keychain.scriptedStatuses[.replace] = [errSecItemNotFound]
        keychain.scriptedStatuses[.insert] = [errSecDuplicateItem]
        keychain.plant(key: key(for: reference), secret: syntheticSecret)

        try service.updateCredential(replacementSecret, reference: reference)

        XCTAssertEqual(keychain.callCount(.replace), 2)
        XCTAssertEqual(keychain.callCount(.insert), 1)
        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)
    }

    func testUpdateRecoveryIsBoundedWhenInsertKeepsReportingDuplicate() {
        let reference = UUID().uuidString
        keychain.forcedStatus[.replace] = errSecItemNotFound
        keychain.forcedStatus[.insert] = errSecDuplicateItem

        XCTAssertThrowsError(try service.updateCredential(syntheticSecret, reference: reference)) { error in
            XCTAssertEqual(error as? CredentialError, .storage(.update, errSecItemNotFound))
        }
        XCTAssertEqual(keychain.callCount(.insert), 1)
        XCTAssertEqual(keychain.callCount(.replace), 2)
    }

    func testDeleteRemovesSecretAndIsIdempotent() throws {
        let reference = try service.addCredential(syntheticSecret)

        try service.deleteCredential(reference: reference)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .absent)
        XCTAssertTrue(keychain.items.isEmpty)

        try service.deleteCredential(reference: reference)
        try service.deleteCredential(reference: UUID().uuidString)
    }

    func testEmptySecretIsRejectedForAddAndUpdateWithoutTouchingKeychain() throws {
        let reference = try service.addCredential(syntheticSecret)
        let insertsBefore = keychain.callCount(.insert)

        XCTAssertThrowsError(try service.addCredential(Data())) { error in
            XCTAssertEqual(error as? CredentialError, .emptySecret)
        }
        XCTAssertThrowsError(try service.updateCredential(Data(), reference: reference)) { error in
            XCTAssertEqual(error as? CredentialError, .emptySecret)
        }
        XCTAssertEqual(keychain.callCount(.insert), insertsBefore)
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
    }

    func testMalformedReferencesAreRejectedBeforeTouchingKeychain() {
        let malformedReferences = ["", "not-a-uuid", "synthetic-provider-credential-one", " "]
        for reference in malformedReferences {
            XCTAssertThrowsError(try service.updateCredential(syntheticSecret, reference: reference)) { XCTAssertEqual($0 as? CredentialError, .invalidReference) }
            XCTAssertThrowsError(try service.deleteCredential(reference: reference)) { XCTAssertEqual($0 as? CredentialError, .invalidReference) }
            XCTAssertThrowsError(try service.credentialStatus(reference: reference)) { XCTAssertEqual($0 as? CredentialError, .invalidReference) }
            XCTAssertThrowsError(try service.resolveSecret(reference: reference)) { XCTAssertEqual($0 as? CredentialError, .invalidReference) }
        }
        for operation in [FakeKeychainOperation.insert, .replace, .inspect, .read, .remove] {
            XCTAssertEqual(keychain.callCount(operation), 0)
        }
    }

    func testReferencesAreScopedToServiceName() throws {
        let reference = try service.addCredential(syntheticSecret)
        let otherService = CredentialService(serviceName: "com.boldfield.ohand.tests.credentials.other", keychain: keychain)

        XCTAssertEqual(try otherService.credentialStatus(reference: reference), .absent)
    }

    // MARK: status and invalidation

    func testStatusDistinguishesPresentAbsentAndInvalidated() throws {
        let present = try service.addCredential(syntheticSecret)
        let absent = UUID().uuidString
        let invalidated = try service.addCredential(syntheticSecret)

        keychain.scriptedStatuses[.inspect] = [errSecSuccess, errSecItemNotFound, errSecDecode]
        XCTAssertEqual(try service.credentialStatus(reference: present), .present)
        XCTAssertEqual(try service.credentialStatus(reference: absent), .absent)
        XCTAssertEqual(try service.credentialStatus(reference: invalidated), .invalidated)
    }

    func testUnreadableItemStatusesMapToInvalidated() throws {
        let reference = try service.addCredential(syntheticSecret)
        for status in [errSecDecode, errSecAuthFailed] {
            keychain.forcedStatus[.inspect] = status
            XCTAssertEqual(try service.credentialStatus(reference: reference), .invalidated, "status \(status)")
        }
    }

    func testStatusPathNeverReadsSecretBytes() throws {
        let reference = try service.addCredential(syntheticSecret)
        let emptyValue = UUID().uuidString
        keychain.plant(key: key(for: emptyValue), secret: Data())

        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
        XCTAssertEqual(try service.credentialStatus(reference: UUID().uuidString), .absent)
        XCTAssertEqual(try service.credentialStatus(reference: emptyValue), .present)
        keychain.forcedStatus[.inspect] = errSecDecode
        XCTAssertEqual(try service.credentialStatus(reference: reference), .invalidated)

        XCTAssertEqual(keychain.callCount(.inspect), 4)
        XCTAssertEqual(keychain.callCount(.read), 0, "health queries must not read the secret")
    }

    func testResolveFailsExplicitlyForAbsentAndInvalidatedCredentials() throws {
        let absent = UUID().uuidString
        let emptyValue = UUID().uuidString
        let undecodable = try service.addCredential(syntheticSecret)
        keychain.plant(key: key(for: emptyValue), secret: Data())

        XCTAssertThrowsError(try service.resolveSecret(reference: absent)) {
            XCTAssertEqual($0 as? CredentialError, .notFound)
        }
        XCTAssertThrowsError(try service.resolveSecret(reference: emptyValue)) {
            XCTAssertEqual($0 as? CredentialError, .invalidated)
        }
        keychain.forcedStatus[.read] = errSecDecode
        XCTAssertThrowsError(try service.resolveSecret(reference: undecodable)) {
            XCTAssertEqual($0 as? CredentialError, .invalidated)
        }
    }

    func testResolveReturnsCurrentSecretAfterUpdate() throws {
        let reference = try service.addCredential(syntheticSecret)
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
        try service.updateCredential(replacementSecret, reference: reference)
        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)
    }

    // MARK: locked and storage errors

    func testLockedDeviceIsTransientAndNeverReportedAsInvalidated() throws {
        let reference = try service.addCredential(syntheticSecret)

        keychain.forcedStatus[.inspect] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try service.credentialStatus(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.status))
        }
        keychain.forcedStatus[.read] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try service.resolveSecret(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.resolve))
        }

        keychain.forcedStatus[.replace] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try service.updateCredential(replacementSecret, reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.update))
        }
        keychain.forcedStatus[.remove] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try service.deleteCredential(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.delete))
        }
        keychain.forcedStatus[.insert] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try service.addCredential(syntheticSecret)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.add))
        }

        keychain.forcedStatus = [:]
        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
    }

    func testStorageStatusesAreNormalizedPerOperation() throws {
        let reference = try service.addCredential(syntheticSecret)
        let storageStatuses: [OSStatus] = [errSecMissingEntitlement, errSecNotAvailable, errSecAllocate, 424_242]

        for status in storageStatuses {
            keychain.forcedStatus = [.insert: status, .replace: status, .inspect: status, .read: status, .remove: status]

            XCTAssertThrowsError(try service.addCredential(syntheticSecret)) {
                XCTAssertEqual($0 as? CredentialError, .storage(.add, status))
            }
            XCTAssertThrowsError(try service.updateCredential(replacementSecret, reference: reference)) {
                XCTAssertEqual($0 as? CredentialError, .storage(.update, status))
            }
            XCTAssertThrowsError(try service.deleteCredential(reference: reference)) {
                XCTAssertEqual($0 as? CredentialError, .storage(.delete, status))
            }
            XCTAssertThrowsError(try service.credentialStatus(reference: reference)) {
                XCTAssertEqual($0 as? CredentialError, .storage(.status, status))
            }
            XCTAssertThrowsError(try service.resolveSecret(reference: reference)) {
                XCTAssertEqual($0 as? CredentialError, .storage(.resolve, status))
            }
        }
        keychain.forcedStatus = [:]
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
    }

    func testFailedStorageLeavesPreviousSecretIntact() throws {
        let reference = try service.addCredential(syntheticSecret)
        keychain.forcedStatus[.replace] = errSecMissingEntitlement

        XCTAssertThrowsError(try service.updateCredential(replacementSecret, reference: reference))
        keychain.forcedStatus = [:]
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
    }

    func testStatusCodesRoundTripThroughJSONAsPlainWords() throws {
        let encoded = try JSONEncoder().encode([CredentialStatus.present, .absent, .invalidated])
        XCTAssertEqual(String(data: encoded, encoding: .utf8), "[\"present\",\"absent\",\"invalidated\"]")
    }
}
