import Foundation
import Security
import XCTest
@testable import OhAndServices

final class ProviderCredentialEntryTests: XCTestCase {
    private let syntheticSecret = "synthetic-credential-secret-one"
    private let replacementSecret = "synthetic-credential-secret-two"

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func makeEntry(authenticated: Bool, keychain: FakeKeychain) -> ProviderCredentialEntry {
        let credentials = CredentialService(
            serviceName: "com.boldfield.ohand.tests.credential-entry.\(UUID().uuidString)", keychain: keychain)
        return ProviderCredentialEntry(store: credentials, session: makeSession(authenticated: authenticated))
    }

    func testAnAuthenticatedSessionStoresReplacesReportsAndDeletesWithoutReturningTheSecret() throws {
        let keychain = FakeKeychain()
        let entry = makeEntry(authenticated: true, keychain: keychain)

        let reference = try entry.addCredential(Data(syntheticSecret.utf8))
        XCTAssertFalse(reference.contains(syntheticSecret), "the reference does not embed the secret")
        XCTAssertEqual(try entry.credentialStatus(reference: reference), .present)
        XCTAssertEqual(keychain.items.count, 1)

        try entry.updateCredential(Data(replacementSecret.utf8), reference: reference)
        XCTAssertEqual(try entry.credentialStatus(reference: reference), .present)
        XCTAssertEqual(keychain.items.values.map(\.secret), [Data(replacementSecret.utf8)])

        try entry.deleteCredential(reference: reference)
        XCTAssertEqual(try entry.credentialStatus(reference: reference), .absent)
        XCTAssertTrue(keychain.items.isEmpty)
    }

    func testACaptureOnlySessionIsRefusedAndTheKeychainIsNotTouched() throws {
        let keychain = FakeKeychain()
        let authenticatedEntry = makeEntry(authenticated: true, keychain: keychain)
        let reference = try authenticatedEntry.addCredential(Data(syntheticSecret.utf8))
        let callsBefore = FakeKeychainOperation.allCasesForTests.map { keychain.callCount($0) }

        let credentials = CredentialService(
            serviceName: "com.boldfield.ohand.tests.credential-entry.\(UUID().uuidString)", keychain: keychain)
        let captureOnly = ProviderCredentialEntry(store: credentials, session: makeSession(authenticated: false))

        XCTAssertThrowsError(try captureOnly.addCredential(Data(replacementSecret.utf8))) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try captureOnly.updateCredential(Data(replacementSecret.utf8), reference: reference)) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try captureOnly.deleteCredential(reference: reference)) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try captureOnly.credentialStatus(reference: reference)) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertEqual(FakeKeychainOperation.allCasesForTests.map { keychain.callCount($0) }, callsBefore)
        XCTAssertEqual(keychain.items.values.map(\.secret), [Data(syntheticSecret.utf8)])
    }

    func testAFailureRenderingCarriesNeitherTheSecretNorTheReference() throws {
        let keychain = FakeKeychain()
        let entry = makeEntry(authenticated: true, keychain: keychain)
        keychain.forcedStatus[.insert] = errSecInteractionNotAllowed

        XCTAssertThrowsError(try entry.addCredential(Data(syntheticSecret.utf8))) { error in
            XCTAssertFalse("\(error)".contains(syntheticSecret))
            XCTAssertFalse(error.localizedDescription.contains(syntheticSecret))
        }
    }
}

extension FakeKeychainOperation {
    fileprivate static let allCasesForTests: [FakeKeychainOperation] = [.insert, .replace, .inspect, .read, .remove]
}
