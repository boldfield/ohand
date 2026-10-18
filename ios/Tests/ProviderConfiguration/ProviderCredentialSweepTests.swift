import Foundation
import Security
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The core side of a sweep from canned answers. `duringHeld` runs inside the profile read, between the sweep's lease
/// read and its profile read.
private final class FakeSweepCore: ProviderCredentialSweepCore, @unchecked Sendable {
    var resetPending: Result<Bool, Error> = .success(false)
    var held: Result<Set<String>, Error> = .success([])
    var duringHeld: (() -> Void)?
    private(set) var heldReads = 0

    func isResetPending() async throws -> Bool { try resetPending.get() }

    func heldReferences() async throws -> Set<String> {
        heldReads += 1
        duringHeld?()
        return try held.get()
    }
}

private struct SyntheticCoreFailure: Error {}

/// The credential sweep over a real `CredentialService` and an in-memory Keychain. A second `CredentialService` over the
/// same Keychain stands in for the next process: it shares every stored item and none of the leases.
final class ProviderCredentialSweepTests: XCTestCase {
    private static let secret = Data("synthetic-sweep-secret".utf8)

    private var keychain: FakeKeychain!
    private var serviceName: String!
    private var core: FakeSweepCore!
    private var session: ReadAuthenticationSession!

    override func setUp() {
        super.setUp()
        keychain = FakeKeychain()
        serviceName = "com.boldfield.ohand.tests.credential-sweep.\(UUID().uuidString)"
        core = FakeSweepCore()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    private func process() -> CredentialService { CredentialService(serviceName: serviceName, keychain: keychain) }

    private func sweep(_ credentials: CredentialService) async -> ProviderCredentialSweepOutcome {
        await ProviderCredentialSweep(store: credentials, core: core, session: session).run()
    }

    private var stored: Set<String> { Set(keychain.items.keys.filter { $0.service == serviceName }.map(\.account)) }

    private func plantOrphan() -> String {
        let reference = UUID().uuidString
        keychain.plant(key: KeychainItemKey(service: serviceName, account: reference), secret: Self.secret)
        return reference
    }

    // MARK: interrupted saves

    func testTwoInterruptedSavesOfOneProfileKeepOnlyTheCommittedSecretAfterARestart() async throws {
        let firstRun = process()
        let committed = try firstRun.addCredential(Self.secret)
        let lost = try firstRun.addCredential(Self.secret)
        XCTAssertEqual(firstRun.leasedReferences(), [committed, lost], "both saves were still waiting on the core")

        // The process ends; the core committed exactly one of the two versions of the profile.
        core.held = .success([committed])
        let outcome = await sweep(process())

        XCTAssertEqual(outcome, .swept(deleted: 1, failed: 0))
        XCTAssertEqual(stored, [committed])
    }

    func testAnInterruptionBeforeTheInsertLeavesNothingToSweepAndNoLease() async {
        let firstRun = process()
        keychain.forcedStatus[.insert] = errSecInteractionNotAllowed
        XCTAssertThrowsError(try firstRun.addCredential(Self.secret))
        XCTAssertEqual(firstRun.leasedReferences(), [], "a lease taken for an insert that failed ends with it")
        keychain.forcedStatus[.insert] = nil

        let outcome = await sweep(process())
        XCTAssertEqual(outcome, .swept(deleted: 0, failed: 0))
        XCTAssertEqual(stored, [])
    }

    func testAnInterruptionAfterTheInsertLeavesASecretTheNextVisitDeletes() async throws {
        let reference = try process().addCredential(Self.secret)
        XCTAssertEqual(stored, [reference])

        let outcome = await sweep(process())
        XCTAssertEqual(outcome, .swept(deleted: 1, failed: 0))
        XCTAssertEqual(stored, [], "no profile holds it and no live process leases it")
    }

    func testAnInterruptionDuringTheCommitIsSettledByWhetherTheCoreCommitted() async throws {
        let committed = try process().addCredential(Self.secret)
        let neverCommitted = try process().addCredential(Self.secret)
        core.held = .success([committed])

        let outcome = await sweep(process())
        XCTAssertEqual(outcome, .swept(deleted: 1, failed: 0))
        XCTAssertEqual(stored, [committed])
        XCTAssertFalse(stored.contains(neverCommitted))
    }

    func testAnInterruptionAfterTheCommitBeforeTheLeaseIsReleasedKeepsTheSecret() async throws {
        let firstRun = process()
        let reference = try firstRun.addCredential(Self.secret)
        core.held = .success([reference])
        let outcome1 = await sweep(firstRun)
        XCTAssertEqual(outcome1, .swept(deleted: 0, failed: 0), "leased and held in the same process")

        // The process ends before the lease is released.
        let outcome2 = await sweep(process())
        XCTAssertEqual(outcome2, .swept(deleted: 0, failed: 0))
        XCTAssertEqual(stored, [reference])
    }

    // MARK: races and refusals

    func testASweepRacingASaveInTheSameProcessDeletesNothingLive() async throws {
        let credentials = process()
        let beingSaved = try credentials.addCredential(Self.secret)
        let orphan = plantOrphan()
        var addedDuringTheSweep: String?
        // Between the lease read and the profile read: the save commits and releases its lease, and another secret
        // is stored after the Keychain was listed.
        core.duringHeld = { [unowned self] in
            core.held = .success([beingSaved])
            credentials.releaseLease(reference: beingSaved)
            addedDuringTheSweep = try? credentials.addCredential(Self.secret)
        }

        let outcome = await sweep(credentials)

        XCTAssertEqual(outcome, .swept(deleted: 1, failed: 0))
        let added = try XCTUnwrap(addedDuringTheSweep)
        XCTAssertEqual(stored, [beingSaved, added])
        XCTAssertFalse(stored.contains(orphan))
    }

    func testASecretReleasedAndCommittedBeforeTheLeaseReadIsKeptBecauseTheCoreHoldsIt() async throws {
        let credentials = process()
        let reference = try credentials.addCredential(Self.secret)
        credentials.releaseLease(reference: reference)
        core.held = .success([reference.lowercased()])

        let outcome3 = await sweep(credentials)

        XCTAssertEqual(outcome3, .swept(deleted: 0, failed: 0), "a held reference matches in any case")
        XCTAssertEqual(stored, [reference])
    }

    func testAListingFailureDeletesNothing() async {
        let orphan = plantOrphan()
        keychain.forcedStatus[.accounts] = errSecInteractionNotAllowed
        let outcome4 = await sweep(process())
        XCTAssertEqual(outcome4, .skippedUnreadable, "before the first unlock the listing is an error")
        XCTAssertEqual(core.heldReads, 0)
        keychain.forcedStatus[.accounts] = nil

        core.held = .failure(SyntheticCoreFailure())
        let outcome5 = await sweep(process())
        XCTAssertEqual(outcome5, .skippedUnreadable)
        XCTAssertEqual(stored, [orphan])
        XCTAssertEqual(keychain.callCount(.remove), 0)
    }

    func testAPendingResetSkipsThePass() async {
        let orphan = plantOrphan()
        core.resetPending = .success(true)
        let outcome6 = await sweep(process())
        XCTAssertEqual(outcome6, .skippedForReset)
        core.resetPending = .failure(SyntheticCoreFailure())
        let outcome7 = await sweep(process())
        XCTAssertEqual(outcome7, .skippedUnreadable, "an unreadable reset state counts as pending")
        XCTAssertEqual(keychain.callCount(.accounts), 0)
        XCTAssertEqual(core.heldReads, 0)
        XCTAssertEqual(stored, [orphan])
    }

    func testALockedSessionDeletesNothing() async {
        let orphan = plantOrphan()
        core.duringHeld = { [unowned self] in session.relock(.explicit) }
        let outcome8 = await sweep(process())
        XCTAssertEqual(outcome8, .skippedLocked, "a relock during the reads stops the pass")
        let outcome9 = await sweep(process())
        XCTAssertEqual(outcome9, .skippedLocked, "a capture-only session does not start one")
        XCTAssertEqual(stored, [orphan])
    }

    func testAFailedDeletionIsCountedAndLeftForTheNextSweep() async {
        let orphan = plantOrphan()
        keychain.forcedStatus[.remove] = errSecInteractionNotAllowed
        let outcome10 = await sweep(process())
        XCTAssertEqual(outcome10, .swept(deleted: 0, failed: 1))
        keychain.forcedStatus[.remove] = nil
        let outcome11 = await sweep(process())
        XCTAssertEqual(outcome11, .swept(deleted: 1, failed: 0))
        XCTAssertFalse(stored.contains(orphan))
    }

    // MARK: listing and leases

    func testTheListingReadsNoSecretAndLeavesOutForeignItems() throws {
        let credentials = process()
        let reference = try credentials.addCredential(Self.secret)
        keychain.plant(key: KeychainItemKey(service: serviceName, account: "not-a-reference"), secret: Self.secret)
        keychain.plant(key: KeychainItemKey(service: "another.service", account: UUID().uuidString), secret: Self.secret)

        XCTAssertEqual(try credentials.listReferences(), [reference])
        XCTAssertEqual(keychain.callCount(.read), 0, "no secret bytes were read")
    }

    func testALeaseLastsFromBeforeTheInsertUntilReleaseOrDeletion() throws {
        let credentials = process()
        let added = try credentials.addCredential(Self.secret)
        XCTAssertEqual(credentials.leasedReferences(), [added])
        credentials.releaseLease(reference: added.lowercased())
        XCTAssertEqual(credentials.leasedReferences(), [], "released under any spelling of the reference")

        let deleted = try credentials.addCredential(Self.secret)
        try credentials.deleteCredential(reference: deleted)
        XCTAssertEqual(credentials.leasedReferences(), [], "deleting ends the lease")

        let recreated = UUID().uuidString
        try credentials.updateCredential(Self.secret, reference: recreated)
        XCTAssertEqual(credentials.leasedReferences(), [recreated], "an update that creates the item leases it too")
        try credentials.updateCredential(Self.secret, reference: added)
        XCTAssertEqual(credentials.leasedReferences(), [recreated], "an update in place takes no lease")
    }
}
