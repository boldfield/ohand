import XCTest
@testable import OhAndServices

final class CredentialServiceTests: XCTestCase {
    var credentialService: CredentialService!
    var fakeKeychain: FakeKeychain!
    let testKeychainService = "com.boldfield.ohand.test.credentials"

    override func setUp() {
        super.setUp()
        fakeKeychain = FakeKeychain()
        credentialService = CredentialService(keychainService: testKeychainService, keychain: fakeKeychain)
    }

    override func tearDown() {
        fakeKeychain.clear()
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

        let retrieved = try credentialService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, updated2, "Should have latest update")
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

    func testStatusDistinguishesAbsentFromPresent() throws {
        let absentReference = UUID().uuidString
        let absentStatus = try credentialService.credentialStatus(reference: absentReference)
        XCTAssertEqual(absentStatus, .absent, "Never-added credential should be absent")

        let secret = "test-secret".data(using: .utf8)!
        let addedReference = try credentialService.addCredential(secret)
        let presentStatus = try credentialService.credentialStatus(reference: addedReference)
        XCTAssertEqual(presentStatus, .present, "Added credential should be present")
    }

    func testStatusDistinguishesInvalidatedFromPresent() throws {
        let secret = "test-secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        let presentStatus = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(presentStatus, .present, "Fresh credential should be present")

        fakeKeychain.markAsInvalidated(reference)

        let invalidatedStatus = try credentialService.credentialStatus(reference: reference)
        XCTAssertEqual(invalidatedStatus, .invalidated, "Invalidated credential should return invalidated status")
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

        let accessibility = fakeKeychain.getAccessibilityClass(for: reference)
        XCTAssertEqual(
            accessibility,
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String,
            "Credential should use AfterFirstUnlockThisDeviceOnly accessibility class"
        )
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

        let freshService = CredentialService(keychainService: testKeychainService, keychain: fakeKeychain)
        let status = try freshService.credentialStatus(reference: reference)
        XCTAssertEqual(status, .present, "Credential should persist through fresh service instance")

        let retrieved = try freshService.retrieveCredential(reference: reference)
        XCTAssertEqual(retrieved, secret, "Credential value should be retrievable from fresh instance")
    }

    // MARK: - Lock and Error Handling Tests

    func testLockErrorDuringStatusCheck() throws {
        let secret = "test-secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextCopyMatchingStatus = errSecInteractionNotAllowed

        do {
            _ = try credentialService.credentialStatus(reference: reference)
            XCTFail("Should throw statusError for lock condition")
        } catch let error as CredentialError {
            guard case .statusError(let msg) = error else {
                XCTFail("Should throw statusError, got \(error)")
                return
            }
            XCTAssertTrue(msg.contains("locked") || msg.contains("Keychain"), "Error should explain the lock issue")
            XCTAssertFalse(msg.contains("secret"), "Error should not leak credential")
        }
    }

    // MARK: - Storage Error Handling Tests

    func testAddWithInteractionNotAllowed() throws {
        let secret = "test-secret".data(using: .utf8)!

        fakeKeychain.nextAddStatus = errSecInteractionNotAllowed

        do {
            _ = try credentialService.addCredential(secret)
            XCTFail("Should throw addFailed for lock condition")
        } catch let error as CredentialError {
            guard case .addFailed(let msg) = error else {
                XCTFail("Should throw addFailed, got \(error)")
                return
            }
            XCTAssertTrue(msg.contains("locked") || msg.contains("Keychain"), "Error should explain the lock issue")
            XCTAssertFalse(msg.contains(String(describing: secret)), "Error should not leak credential")
        }
    }

    func testCredentialErrorsDoNotContainSecrets() throws {
        let secret = "super-secret-value".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)
        try credentialService.deleteCredential(reference: reference)

        do {
            _ = try credentialService.retrieveCredential(reference: reference)
            XCTFail("Should throw keyNotFound")
        } catch let error as CredentialError {
            guard let description = error.errorDescription else {
                XCTFail("Error should have description")
                return
            }
            XCTAssertFalse(description.contains("super-secret"), "Error description should not contain secret")
            XCTAssertFalse(description.contains("secret-value"), "Error description should not contain secret")
        }
    }

    func testRetrieveWithLockError() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextCopyMatchingStatus = errSecInteractionNotAllowed

        do {
            _ = try credentialService.retrieveCredential(reference: reference)
            XCTFail("Should throw error for lock condition")
        } catch let error as CredentialError {
            let errorMessage = error.errorDescription ?? ""
            XCTAssertFalse(errorMessage.contains("secret"), "Error should not leak credential")
        }
    }

    func testStatusWithAuthFailure() throws {
        let secret = "api-key-12345-secret-token".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextCopyMatchingStatus = errSecAuthFailed

        do {
            _ = try credentialService.credentialStatus(reference: reference)
            XCTFail("Should throw error for auth failure")
        } catch let error as CredentialError {
            let errorMessage = error.errorDescription ?? ""
            XCTAssertFalse(errorMessage.contains("api-key"), "Error should not leak credential")
            XCTAssertFalse(errorMessage.contains("secret-token"), "Error should not leak credential")
            XCTAssertFalse(errorMessage.contains("12345"), "Error should not leak credential")
        }
    }

    func testAddWithAuthFailure() throws {
        let sensitiveSecret = "oauth-token-xyz789".data(using: .utf8)!

        fakeKeychain.nextAddStatus = errSecAuthFailed

        do {
            _ = try credentialService.addCredential(sensitiveSecret)
            XCTFail("Should throw error for auth failure")
        } catch let error as CredentialError {
            let errorMessage = error.errorDescription ?? ""
            XCTAssertFalse(errorMessage.contains("oauth-token"), "Error should not leak credential")
            XCTAssertFalse(errorMessage.contains("xyz789"), "Error should not leak credential")
        }
    }

    func testUpdateWithAuthFailure() throws {
        let originalSecret = "original-secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(originalSecret)

        let sensitiveUpdate = "new-api-secret-key".data(using: .utf8)!
        fakeKeychain.nextUpdateStatus = errSecAuthFailed

        do {
            try credentialService.updateCredential(sensitiveUpdate, reference: reference)
            XCTFail("Should throw error for auth failure")
        } catch let error as CredentialError {
            let errorMessage = error.errorDescription ?? ""
            XCTAssertFalse(errorMessage.contains("api-secret"), "Error should not leak credential")
            XCTAssertFalse(errorMessage.contains("secret-key"), "Error should not leak credential")
        }
    }

    func testDeleteWithAuthFailure() throws {
        let sensitiveSecret = "delete-test-secret-123".data(using: .utf8)!
        let reference = try credentialService.addCredential(sensitiveSecret)

        fakeKeychain.nextDeleteStatus = errSecAuthFailed

        do {
            try credentialService.deleteCredential(reference: reference)
            XCTFail("Should throw error for auth failure")
        } catch let error as CredentialError {
            let errorMessage = error.errorDescription ?? ""
            XCTAssertFalse(errorMessage.contains("delete-test"), "Error should not leak credential")
            XCTAssertFalse(errorMessage.contains("secret-123"), "Error should not leak credential")
        }
    }

    // MARK: - Additional Error Scenario Tests

    func testUpdateWithUserCanceledError() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextUpdateStatus = errSecUserCanceled

        do {
            try credentialService.updateCredential(secret, reference: reference)
            XCTFail("Should throw error for user cancel")
        } catch let error as CredentialError {
            guard case .updateFailed = error else {
                XCTFail("Should throw updateFailed")
                return
            }
        }
    }

    func testDeleteWithUserCanceledError() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextDeleteStatus = errSecUserCanceled

        do {
            try credentialService.deleteCredential(reference: reference)
            XCTFail("Should throw error for user cancel")
        } catch let error as CredentialError {
            guard case .deleteFailed = error else {
                XCTFail("Should throw deleteFailed")
                return
            }
        }
    }

    func testStatusWithUnexpectedError() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextCopyMatchingStatus = errSecUnimplemented

        do {
            _ = try credentialService.credentialStatus(reference: reference)
            XCTFail("Should throw error for unexpected status")
        } catch let error as CredentialError {
            guard case .unexpectedStatus = error else {
                XCTFail("Should throw unexpectedStatus")
                return
            }
        }
    }

    func testAddWithUnexpectedError() throws {
        let secret = "secret".data(using: .utf8)!

        fakeKeychain.nextAddStatus = errSecUnimplemented

        do {
            _ = try credentialService.addCredential(secret)
            XCTFail("Should throw error for unexpected status")
        } catch let error as CredentialError {
            guard case .unexpectedStatus = error else {
                XCTFail("Should throw unexpectedStatus")
                return
            }
        }
    }

    func testRetrieveWithUserCanceledError() throws {
        let secret = "secret".data(using: .utf8)!
        let reference = try credentialService.addCredential(secret)

        fakeKeychain.nextCopyMatchingStatus = errSecUserCanceled

        do {
            _ = try credentialService.retrieveCredential(reference: reference)
            XCTFail("Should throw error for user cancel")
        } catch let error as CredentialError {
            guard case .statusError = error else {
                XCTFail("Should throw statusError")
                return
            }
        }
    }

}
