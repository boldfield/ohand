import Foundation
import Security
import XCTest
@testable import OhAndServices

/// `KeychainResetCredentialRemover` against the real simulator Keychain, through the production `CredentialService` for
/// every write: the identity it reads is the Keychain's own metadata, and the removal it performs is conditioned on it
/// by the Keychain itself. The host app carries Keychain entitlements (`Config/OhAndApp.entitlements`); without them
/// every call fails with errSecMissingEntitlement (-34018) and these tests fail loudly rather than skip.
final class ResetCredentialRemoverKeychainTests: XCTestCase {
    private let syntheticSecret = Data("synthetic-reset-credential-one".utf8)
    private let replacementSecret = Data("synthetic-reset-credential-two".utf8)
    private var serviceName = ""
    private var references: [String] = []

    override func setUp() {
        super.setUp()
        serviceName = "com.boldfield.ohand.tests.reset.credentials.\(UUID().uuidString)"
        references = []
    }

    override func tearDown() {
        let cleanup = CredentialService(serviceName: serviceName)
        for reference in references {
            try? cleanup.deleteCredential(reference: reference)
        }
        super.tearDown()
    }

    private func makeCredentials() -> (CredentialService, KeychainResetCredentialRemover) {
        (CredentialService(serviceName: serviceName), KeychainResetCredentialRemover(serviceName: serviceName))
    }

    private func add(_ service: CredentialService, _ secret: Data) throws -> String {
        let reference = try service.addCredential(secret)
        references.append(reference)
        return reference
    }

    /// The Keychain stamps writes with its clock; a write in the same instant as the previous one would read as the same
    /// identity, so leave a moment between writes whose identities a test compares.
    private func letTheClockMove() {
        Thread.sleep(forTimeInterval: 0.01)
    }

    func testTheIdentityNamesTheStoredSecretAndEveryWriteMovesIt() throws {
        let (service, remover) = makeCredentials()
        let reference = try add(service, syntheticSecret)

        let atTheTap = try XCTUnwrap(try remover.secretIdentity(reference: reference))
        XCTAssertEqual(try remover.secretIdentity(reference: reference), atTheTap, "reading moves nothing")
        XCTAssertNil(try remover.secretIdentity(reference: UUID().uuidString), "a reference without an item has no secret")

        letTheClockMove()
        try service.updateCredential(replacementSecret, reference: reference)
        let afterUpdate = try XCTUnwrap(try remover.secretIdentity(reference: reference))
        XCTAssertNotEqual(afterUpdate, atTheTap, "an overwrite behind the same reference is a different secret")
        XCTAssertEqual(afterUpdate.createdAt, atTheTap.createdAt, "the item itself is the same one")
        XCTAssertGreaterThan(afterUpdate.modifiedAt, atTheTap.modifiedAt)

        try service.deleteCredential(reference: reference)
        XCTAssertNil(try remover.secretIdentity(reference: reference))

        letTheClockMove()
        try service.updateCredential(syntheticSecret, reference: reference)
        let reBound = try XCTUnwrap(try remover.secretIdentity(reference: reference))
        XCTAssertNotEqual(reBound, atTheTap, "a reference re-bound after a delete holds a new secret")
        XCTAssertGreaterThan(reBound.createdAt, atTheTap.createdAt)
    }

    func testRemovalTakesOnlyTheSecretThatWasStoredWhenTheChoiceBoundIt() throws {
        let (service, remover) = makeCredentials()
        let reference = try add(service, syntheticSecret)
        let atTheTap = try XCTUnwrap(try remover.secretIdentity(reference: reference))

        // The user enters a new secret behind the same reference before the removal is retried.
        letTheClockMove()
        try service.updateCredential(replacementSecret, reference: reference)

        try remover.deleteCredential(reference: reference, identity: atTheTap)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .present, "the secret entered since stays")
        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)

        let current = try XCTUnwrap(try remover.secretIdentity(reference: reference))
        try remover.deleteCredential(reference: reference, identity: current)
        XCTAssertEqual(try service.credentialStatus(reference: reference), .absent, "the secret the choice binds goes")
        try remover.deleteCredential(reference: reference, identity: current)
        XCTAssertNil(try remover.secretIdentity(reference: reference), "removing an absent secret succeeds")
    }

    func testRemovalLeavesAReferenceReBoundAfterADeleteAlone() throws {
        let (service, remover) = makeCredentials()
        let reference = try add(service, syntheticSecret)
        let atTheTap = try XCTUnwrap(try remover.secretIdentity(reference: reference))
        try service.deleteCredential(reference: reference)
        letTheClockMove()
        try service.updateCredential(replacementSecret, reference: reference)

        try remover.deleteCredential(reference: reference, identity: atTheTap)

        XCTAssertEqual(try service.resolveSecret(reference: reference), replacementSecret)
    }

    func testAnInvalidReferenceIsRefusedWithoutTouchingTheKeychain() {
        let (_, remover) = makeCredentials()
        let identity = ResetSecretIdentity(createdAtBits: 1, modifiedAtBits: 1)
        XCTAssertThrowsError(try remover.secretIdentity(reference: "not-a-reference")) {
            XCTAssertEqual($0 as? CredentialError, .invalidReference)
        }
        XCTAssertThrowsError(try remover.deleteCredential(reference: "not-a-reference", identity: identity)) {
            XCTAssertEqual($0 as? CredentialError, .invalidReference)
        }
    }
}

/// `KeychainResetCredentialRemover` over a scripted Keychain, for the statuses the simulator cannot be made to return.
final class ResetCredentialRemoverStatusTests: XCTestCase {
    private let reference = UUID().uuidString
    private let identity = ResetSecretIdentity(createdAtBits: 10, modifiedAtBits: 20)
    private let other = ResetSecretIdentity(createdAtBits: 10, modifiedAtBits: 30)

    private func makeRemover(_ keychain: ScriptedResetSecretKeychain) -> KeychainResetCredentialRemover {
        KeychainResetCredentialRemover(serviceName: "ohand.reset.tests.scripted", keychain: keychain)
    }

    func testALockedDeviceAndAStorageFailureAreReportedNotTakenAsAbsent() {
        let keychain = ScriptedResetSecretKeychain()
        keychain.lookups = [ResetSecretLookup(status: errSecInteractionNotAllowed, identity: nil)]
        XCTAssertThrowsError(try makeRemover(keychain).secretIdentity(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.status))
        }

        keychain.lookups = [ResetSecretLookup(status: errSecIO, identity: nil)]
        XCTAssertThrowsError(try makeRemover(keychain).secretIdentity(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .storage(.status, errSecIO))
        }

        keychain.lookups = [ResetSecretLookup(status: errSecInteractionNotAllowed, identity: nil)]
        XCTAssertThrowsError(try makeRemover(keychain).deleteCredential(reference: reference, identity: identity)) {
            XCTAssertEqual($0 as? CredentialError, .deviceLocked(.delete))
        }
        XCTAssertEqual(keychain.removals, [], "nothing is removed while the stored secret cannot be identified")

        keychain.lookups = [ResetSecretLookup(status: errSecSuccess, identity: identity)]
        keychain.removalStatuses = [errSecIO]
        XCTAssertThrowsError(try makeRemover(keychain).deleteCredential(reference: reference, identity: identity)) {
            XCTAssertEqual($0 as? CredentialError, .storage(.delete, errSecIO))
        }
    }

    func testRemovalIsSkippedWhenADifferentSecretIsStoredAndNeverFallsBackToABlindRemoval() throws {
        let keychain = ScriptedResetSecretKeychain()
        keychain.lookups = [ResetSecretLookup(status: errSecSuccess, identity: other)]
        try makeRemover(keychain).deleteCredential(reference: reference, identity: identity)
        XCTAssertEqual(keychain.removals, [], "a secret entered since the tap is not the bound one")

        // The secret changed between the check and the removal: the Keychain matched nothing, which is the same outcome.
        keychain.lookups = [
            ResetSecretLookup(status: errSecSuccess, identity: identity),
            ResetSecretLookup(status: errSecSuccess, identity: other),
        ]
        keychain.removalStatuses = [errSecItemNotFound]
        try makeRemover(keychain).deleteCredential(reference: reference, identity: identity)
        XCTAssertEqual(keychain.removals, [identity], "the removal named the bound identity")

        // The Keychain matched nothing although the bound secret is still there: reported, never removed blindly.
        keychain.removals = []
        keychain.lookups = [
            ResetSecretLookup(status: errSecSuccess, identity: identity),
            ResetSecretLookup(status: errSecSuccess, identity: identity),
        ]
        keychain.removalStatuses = [errSecItemNotFound]
        XCTAssertThrowsError(try makeRemover(keychain).deleteCredential(reference: reference, identity: identity)) {
            XCTAssertEqual($0 as? CredentialError, .storage(.delete, errSecItemNotFound))
        }
        XCTAssertEqual(keychain.removals, [identity])
    }

    func testAnItemWhoseMetadataCannotBeReadIsNotOneResetMayIdentify() {
        let keychain = ScriptedResetSecretKeychain()
        keychain.lookups = [ResetSecretLookup(status: errSecSuccess, identity: nil)]
        XCTAssertThrowsError(try makeRemover(keychain).secretIdentity(reference: reference)) {
            XCTAssertEqual($0 as? CredentialError, .storage(.status, errSecDecode))
        }
    }
}

private final class ScriptedResetSecretKeychain: ResetSecretKeychain {
    var lookups: [ResetSecretLookup] = []
    var removalStatuses: [OSStatus] = []
    var removals: [ResetSecretIdentity] = []

    func identity(of key: KeychainItemKey) -> ResetSecretLookup {
        guard !lookups.isEmpty else { return ResetSecretLookup(status: errSecItemNotFound, identity: nil) }
        return lookups.removeFirst()
    }

    func remove(_ key: KeychainItemKey, identity: ResetSecretIdentity) -> OSStatus {
        removals.append(identity)
        guard !removalStatuses.isEmpty else { return errSecSuccess }
        return removalStatuses.removeFirst()
    }
}
