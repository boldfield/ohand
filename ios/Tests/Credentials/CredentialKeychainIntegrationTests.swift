import XCTest
import Security
@testable import OhAndServices

/// Exercises `CredentialService` against the real simulator Keychain with synthetic secrets. The host app must
/// carry Keychain entitlements (`Config/OhAndApp.entitlements`, ad-hoc signed in CI); without them every call fails
/// with errSecMissingEntitlement (-34018) and these tests fail loudly rather than skip.
///
/// The simulator never locks, so lock behavior is not observed here: the locked-device mapping is covered with an
/// injected errSecInteractionNotAllowed in `CredentialServiceTests`, and the device lock matrix belongs to P09.
final class CredentialKeychainIntegrationTests: XCTestCase {
    private let syntheticSecret = Data("synthetic-integration-credential-one".utf8)
    private let replacementSecret = Data("synthetic-integration-credential-two".utf8)
    private var serviceName = ""
    private var references: [String] = []

    override func setUp() {
        super.setUp()
        serviceName = "com.boldfield.ohand.tests.credentials.integration.\(UUID().uuidString)"
        references = []
    }

    override func tearDown() {
        let cleanup = CredentialService(serviceName: serviceName)
        for reference in references {
            try? cleanup.deleteCredential(reference: reference)
        }
        super.tearDown()
    }

    private func rawAttributes(for reference: String) -> (status: OSStatus, attributes: [String: Any]?) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: reference,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        return (status, result as? [String: Any])
    }

    func testAddResolveUpdateDeleteRoundTripAgainstRealKeychain() throws {
        let service = CredentialService(serviceName: serviceName)
        let reference = try service.addCredential(syntheticSecret)
        references.append(reference)

        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)

        try service.updateCredential(replacementSecret, reference: reference)
        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)

        try service.deleteCredential(reference: reference)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .absent)
        XCTAssertThrowsError(try service.resolveSecret(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .notFound)
        }
        try service.deleteCredential(reference: reference)
    }

    func testStoredItemKeepsAfterFirstUnlockThisDeviceOnlyAndIsNotSynchronized() throws {
        let service = CredentialService(serviceName: serviceName)
        let reference = try service.addCredential(syntheticSecret)
        references.append(reference)

        let stored = rawAttributes(for: reference)
        XCTAssertEqual(stored.status, errSecSuccess)
        let attributes = try XCTUnwrap(stored.attributes)
        XCTAssertEqual(
            attributes[kSecAttrAccessible as String] as? String,
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
        XCTAssertNotEqual(attributes[kSecAttrSynchronizable as String] as? Bool, true)

        if let accessGroup = attributes[kSecAttrAccessGroup as String] as? String {
            XCTAssertTrue(
                accessGroup.hasSuffix("com.boldfield.ohand.app"),
                "item must live only in the app's own access group, found \(accessGroup)")
        }
    }

    func testUpdateNormalizesAccessibilityOfAnItemStoredUnderAnotherClass() throws {
        let reference = UUID().uuidString
        references.append(reference)
        let seed: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: reference,
            kSecValueData as String: syntheticSecret,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        XCTAssertEqual(SecItemAdd(seed as CFDictionary, nil), errSecSuccess)

        let service = CredentialService(serviceName: serviceName)
        XCTAssertEqual(try service.resolveSecret(reference: reference), syntheticSecret)
        try service.updateCredential(replacementSecret, reference: reference)

        let attributes = try XCTUnwrap(rawAttributes(for: reference).attributes)
        XCTAssertEqual(
            attributes[kSecAttrAccessible as String] as? String,
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)
        try service.deleteCredential(reference: reference)
        XCTAssertEqual(rawAttributes(for: reference).status, errSecItemNotFound)
    }

    func testCredentialSurvivesRecreatingTheServiceAsRelaunchProxy() throws {
        let reference = try CredentialService(serviceName: serviceName).addCredential(syntheticSecret)
        references.append(reference)

        let relaunched = CredentialService(serviceName: serviceName)
        XCTAssertEqual(try relaunched.credentialStatus(reference: reference), .present)
        XCTAssertEqual(try relaunched.resolveSecret(reference: reference), syntheticSecret)
    }

    func testEmptyStoredValueFailsExplicitlyAtResolveAndRecoversOnUpdate() throws {
        let reference = UUID().uuidString
        references.append(reference)
        let seed: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: reference,
            kSecValueData as String: Data(),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        XCTAssertEqual(SecItemAdd(seed as CFDictionary, nil), errSecSuccess)

        let service = CredentialService(serviceName: serviceName)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
        XCTAssertThrowsError(try service.resolveSecret(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .invalidated)
        }
        try service.updateCredential(replacementSecret, reference: reference)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .present)
    }

    func testServiceNamesIsolateCredentialsFromEachOther() throws {
        let service = CredentialService(serviceName: serviceName)
        let reference = try service.addCredential(syntheticSecret)
        references.append(reference)

        let other = CredentialService(serviceName: serviceName + ".other")
        XCTAssertEqual(try other.credentialStatus(reference: reference), .absent)
    }
}
