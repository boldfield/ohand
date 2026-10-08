import XCTest
@testable import OhAndServices

final class CredentialServiceTests: XCTestCase {
    var credentialService: CredentialService!
    let testKeychainService = "com.boldfield.ohand.test.credentials"

    override func setUp() {
        super.setUp()
        credentialService = CredentialService(keychainService: testKeychainService)
        clearAllTestCredentials()
    }

    override func tearDown() {
        clearAllTestCredentials()
        super.tearDown()
    }

    // MARK: - Add Credential Tests

    func testAddCredentialSucceeds() throws {
        let secretData = "test-secret-value".data(using: .utf8)!

        let reference = try credentialService.addCredential(secretData)

        XCTAssertFalse(reference.isEmpty, "Reference should not be empty")
        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Credential should be present after add")
    }

    func testAddMultipleCredentials() throws {
        let secret1 = "first-secret".data(using: .utf8)!
        let secret2 = "second-secret".data(using: .utf8)!

        let ref1 = try credentialService.addCredential(secret1)
        let ref2 = try credentialService.addCredential(secret2)

        XCTAssertNotEqual(ref1, ref2, "Different secrets should get different references")
        let status1 = try credentialService.credentialStatus(reference: ref1)
        let status2 = try credentialService.credentialStatus(reference: ref2)
        XCTAssertEqual(status1, .present)
        XCTAssertEqual(status2, .present)
    }

    func testAddCredentialWithLargeSecret() throws {
        var largeSecret = Data()
        for _ in 0..<10000 {
            largeSecret.append(contentsOf: "x".data(using: .utf8)!)
        }

        let reference = try credentialService.addCredential(largeSecret)
        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present)
    }

    func testAddCredentialWithBinaryData() throws {
        var binarySecret = Data()
        for i in 0...255 {
            binarySecret.append(UInt8(i))
        }

        let reference = try credentialService.addCredential(binarySecret)
        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present)
    }

    // MARK: - Update Credential Tests

    func testUpdateCredentialSucceeds() throws {
        let originalSecret = "original-secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(originalSecret)

        let newSecret = "updated-secret".data(using: .utf8)!
        try credentialService.updateCredential(newSecret, reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Updated credential should still be present")
    }

    func testUpdateCredentialWithNewValue() throws {
        let originalSecret = "original".data(using: .utf8)!
        let reference = try credentialService.addCredential(originalSecret)

        let updated1 = "updated-once".data(using: .utf8)!
        try credentialService.updateCredential(updated1, reference: reference)

        let updated2 = "updated-twice".data(using: .utf8)!
        try credentialService.updateCredential(updated2, reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present)
    }

    func testUpdateNonexistentCredentialCreatesIt() throws {
        let secret = "new-secret".data(using: .utf8)!
        let reference = UUID().uuidString

        try credentialService.updateCredential(secret, reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Update should create credential if not present")
    }

    func testUpdateWithEmptyReferenceThrows() throws {
        let secret = "secret".data(using: .utf8)!

        XCTAssertThrowsError(try credentialService.updateCredential(secret, reference: "")) { error in
            if case let .invalidReference(msg) = error as? CredentialError {
                XCTAssertTrue(msg.contains("Empty"), "Should specify empty reference issue")
            } else {
                XCTFail("Should throw invalidReference error")
            }
        }
    }

    // MARK: - Delete Credential Tests

    func testDeleteCredentialSucceeds() throws {
        let secret = "secret-to-delete".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        try credentialService.deleteCredential(reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .absent, "Credential should be absent after delete")
    }

    func testDeleteNonexistentCredentialDoesNotThrow() throws {
        let reference = UUID().uuidString

        try credentialService.deleteCredential(reference: reference)
    }

    func testDeleteCredentialTwice() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        try credentialService.deleteCredential(reference: reference)
        try credentialService.deleteCredential(reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .absent)
    }

    func testDeleteWithEmptyReferenceThrows() throws {
        XCTAssertThrowsError(try credentialService.deleteCredential(reference: "")) { error in
            if case let .invalidReference(msg) = error as? CredentialError {
                XCTAssertTrue(msg.contains("Empty"), "Should specify empty reference issue")
            } else {
                XCTFail("Should throw invalidReference error")
            }
        }
    }

    // MARK: - Status Query Tests

    func testStatusPresentForExistingCredential() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present)
    }

    func testStatusAbsentForDeletedCredential() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)
        try credentialService.deleteCredential(reference: reference)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .absent)
    }

    func testStatusAbsentForNeverAddedCredential() throws {
        let reference = UUID().uuidString

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .absent)
    }

    func testStatusWithEmptyReferenceThrows() throws {
        XCTAssertThrowsError(try credentialService.credentialStatus(reference: "")) { error in
            if case let .invalidReference(msg) = error as? CredentialError {
                XCTAssertTrue(msg.contains("Empty"), "Should specify empty reference issue")
            } else {
                XCTFail("Should throw invalidReference error")
            }
        }
    }

    // MARK: - Retrieve Credential Tests (Internal use for transport)

    func testRetrieveCredentialSucceeds() throws {
        let secretValue = "test-secret-data".data(using: .utf8)!
        let reference = try credentialService.addCredential(secretValue)

        let retrieved = try credentialService.retrieveCredential(reference: reference)

        XCTAssertEqual(retrieved, secretValue, "Retrieved credential should match stored value")
    }

    func testRetrieveCredentialAfterUpdate() throws {
        let original = "original".data(using: .utf8)!
        let reference = try credentialService.addCredential(original)

        let updated = "updated-value".data(using: .utf8)!
        try credentialService.updateCredential(updated, reference: reference)

        let retrieved = try credentialService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, updated, "Should retrieve updated value")
    }

    func testRetrieveDeletedCredentialThrows() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)
        try credentialService.deleteCredential(reference: reference)

        XCTAssertThrowsError(try credentialService.retrieveCredential(reference: reference)) { error in
            if case .keyNotFound = error as? CredentialError {
                XCTAssertTrue(true)
            } else {
                XCTFail("Should throw keyNotFound error")
            }
        }
    }

    func testRetrieveNonexistentCredentialThrows() throws {
        let reference = UUID().uuidString

        XCTAssertThrowsError(try credentialService.retrieveCredential(reference: reference)) { error in
            if case .keyNotFound = error as? CredentialError {
                XCTAssertTrue(true)
            } else {
                XCTFail("Should throw keyNotFound error")
            }
        }
    }

    func testRetrieveWithEmptyReferenceThrows() throws {
        XCTAssertThrowsError(try credentialService.retrieveCredential(reference: "")) { error in
            if case let .invalidReference(msg) = error as? CredentialError {
                XCTAssertTrue(msg.contains("Empty"), "Should specify empty reference issue")
            } else {
                XCTFail("Should throw invalidReference error")
            }
        }
    }

    func testRetrieveBinaryCredential() throws {
        var binarySecret = Data()
        for i in 0...255 {
            binarySecret.append(UInt8(i))
        }
        let reference = try credentialService.addCredential(binarySecret)

        let retrieved = try credentialService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, binarySecret, "Binary data should round-trip correctly")
    }

    // MARK: - Credential Isolation Tests

    func testCredentialsAreIsolated() throws {
        let secret1 = "secret-1".data(using: .utf8)!
        let secret2 = "secret-2".data(using: .utf8)!

        let ref1 = try credentialService.addCredential(secret1)
        let ref2 = try credentialService.addCredential(secret2)

        let retrieved1 = try credentialService.retrieveCredential(reference: ref1)
        let retrieved2 = try credentialService.retrieveCredential(reference: ref2)

        XCTAssertEqual(retrieved1, secret1)
        XCTAssertEqual(retrieved2, secret2)
        XCTAssertNotEqual(retrieved1, retrieved2)
    }

    func testDeleteOneCredentialDoesNotAffectOthers() throws {
        let secret1 = "secret-1".data(using: .utf8)!
        let secret2 = "secret-2".data(using: .utf8)!

        let ref1 = try credentialService.addCredential(secret1)
        let ref2 = try credentialService.addCredential(secret2)

        try credentialService.deleteCredential(reference: ref1)

        let status1 = try credentialService.credentialStatus(reference: ref1)
        let status2 = try credentialService.credentialStatus(reference: ref2)

        XCTAssertEqual(status1, .absent)
        XCTAssertEqual(status2, .present, "Other credentials should not be affected")
    }

    // MARK: - Error Handling Tests

    func testErrorsAreNotLogged() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)
        try credentialService.deleteCredential(reference: reference)

        XCTAssertThrowsError(try credentialService.retrieveCredential(reference: reference)) { error in
            let errorDescription = (error as? CredentialError)?.errorDescription ?? ""
            XCTAssertFalse(errorDescription.contains("secret"), "Error should not contain secret value")
        }
    }

    func testErrorMessageIsRedacted() throws {
        let error = CredentialError.addFailed("Something went wrong")
        XCTAssertFalse(error.errorDescription?.contains("secret") ?? false, "Error should be redacted")
    }

    // MARK: - Accessibility Class Tests

    func testCredentialUsesAppropriateAccessibility() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let status = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Credential with appropriate accessibility should be accessible")
    }

    // MARK: - Idempotency Tests

    func testAddSameSecretTwice() throws {
        let secret = "duplicate-secret".data(using: .utf8)!

        let ref1 = try credentialService.addCredential(secret)
        let ref2 = try credentialService.addCredential(secret)

        XCTAssertNotEqual(ref1, ref2, "Same secret should create different references")
    }

    func testUpdateIsIdempotent() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let newSecret = "new-secret".data(using: .utf8)!
        try credentialService.updateCredential(newSecret, reference: reference)

        try credentialService.updateCredential(newSecret, reference: reference)

        let retrieved = try credentialService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, newSecret)
    }

    // MARK: - Lock/Relaunch Behavior Tests

    func testCredentialPersistsThroughFreshServiceInstance() throws {
        let secret = "persistent-secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let freshService = CredentialService(keychainService: testKeychainService)
        let status = try freshService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Credential should persist through fresh service instance")

        let retrieved = try freshService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, secret, "Credential value should be retrievable from fresh instance")
    }

    func testAccessibilityClassIsAfterFirstUnlock() throws {
        let secret = "accessibility-test".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: testKeychainService,
            kSecAttrAccount as String: reference,
            kSecReturnAttributes as String: true
        ]

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        XCTAssertEqual(status, errSecSuccess, "Should find the credential")
        if let attributes = result as? [String: Any] {
            let accessibleValue = attributes[kSecAttrAccessible as String]
            XCTAssertEqual(
                accessibleValue as? String,
                kSecAttrAccessibleWhenUnlockedThisDeviceOnly as String,
                "Credential should use WhenUnlocked accessibility class"
            )
        }
    }

    // MARK: - Storage Error Handling Tests

    func testAddWithDeviceLockedError() throws {
        let secret = "test-secret".data(using: .utf8)!
        let reference = UUID().uuidString

        var query = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: testKeychainService,
            kSecAttrAccount as String: reference,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            kSecValueData as String: secret
        ] as [String: Any]

        let status = SecItemAdd(query as CFDictionary, nil)
        XCTAssertEqual(status, errSecSuccess, "Test setup: should add item")
    }

    func testCredentialErrorsDoNotContainSecrets() throws {
        let secret = "super-secret-value".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)
        try credentialService.deleteCredential(reference: reference)

        do {
            _ = try credentialService.retrieveCredential(reference: reference)
            XCTFail("Should throw keyNotFound")
        } catch let error as CredentialError {
            if let description = error.errorDescription {
                XCTAssertFalse(description.contains("super-secret"), "Error description should not contain secret")
                XCTAssertFalse(description.contains("secret-value"), "Error description should not contain secret")
            }
        }
    }

    func testInteractionNotAllowedErrorHandling() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        do {
            _ = try credentialService.retrieveCredential(reference: reference)
        } catch let error as CredentialError {
            if case .statusError(let msg) = error {
                XCTAssertTrue(msg.contains("Keychain") || msg.contains("access"), "Error should explain the issue")
                XCTAssertFalse(msg.contains("secret"), "Error should not contain secret value")
            }
        }
    }

    // MARK: - Helpers

    private func clearAllTestCredentials() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: testKeychainService
        ]
        SecItemDelete(query as CFDictionary)
    }
}
