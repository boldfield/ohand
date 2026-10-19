import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Drives `ResetSettingsModel` over the production `ResetService`, the production core clients and the real Rust core on
/// a file store, using the reset fixture's real protected-storage files. Only the read-session wrapper is replaced by
/// the model's own scope closure; every reset step, generation and tombstone is the core's. Everything is synthetic.
@MainActor
final class ResetSettingsRealCoreTests: XCTestCase {
    /// A credential remover whose first `failures` deletions fail, as a Keychain that is briefly unavailable does.
    private final class FlakyCredentials: ResetCredentialRemoving, @unchecked Sendable {
        struct Unavailable: Error {}

        private let wrapped: ResetCredentialRemoving
        private let lock = NSLock()
        private var remainingFailures: Int

        init(_ wrapped: ResetCredentialRemoving, failures: Int) {
            self.wrapped = wrapped
            remainingFailures = failures
        }

        func secretIdentity(reference: String) throws -> ResetSecretIdentity? {
            try wrapped.secretIdentity(reference: reference)
        }

        func deleteCredential(reference: String, identity: ResetSecretIdentity) throws {
            lock.lock()
            let fails = remainingFailures > 0
            if fails { remainingFailures -= 1 }
            lock.unlock()
            if fails { throw Unavailable() }
            try wrapped.deleteCredential(reference: reference, identity: identity)
        }
    }

    private let readSession = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())

    /// The production `ResetControl` over the runtime's service and core, with an authenticated read session.
    private func productionModel(_ runtime: RealCoreResetRuntime) throws -> ResetSettingsModel {
        if readSession.scope != .authenticated { readSession.authenticate(reason: "test") { _ in } }
        XCTAssertEqual(readSession.scope, .authenticated)
        let control = ResetControl(
            service: try XCTUnwrap(runtime.service), core: runtime.session.resetClient, session: readSession)
        return ResetSettingsModel(controls: control, readScope: { [readSession] in
            readSession.scope == .authenticated ? .authenticated : .captureOnly
        })
    }

    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreResetWorld!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreResetWorld()
    }

    override func tearDown() {
        world?.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    /// These tests are synchronous on purpose: the fixture waits for the core's events by pumping the main run loop, which
    /// only delivers them from a synchronous test. The model's async calls run through the session's `run`.
    func testAConfirmedResetCompletesAndCaptureResumesUnderTheNewGeneration() throws {
        let oldItem = try world.storage.importVoiceCapture(captureID: "cap-old")
        let runtime = try world.launch()
        let model = try productionModel(runtime)
        XCTAssertEqual(try runtime.generation(ofItem: oldItem), 0)

        model.credentialChoice = .keep
        model.beginConfirmation()
        let confirming = ResetUncheckedBox(model)
        try runtime.session.run { await confirming.value.confirmReset() }

        XCTAssertEqual(model.phase, .completed)
        XCTAssertEqual(world.storage.remnants(of: "cap-old"), [], "the earlier capture's files are gone")
        XCTAssertEqual(try runtime.currentGeneration(), 1)

        let newItem = try runtime.capture(world.textRecord("cap-new"))
        XCTAssertEqual(try runtime.generation(ofItem: newItem), 1, "capture resumed under the new generation")
        runtime.terminate()
    }

    func testAFreshModelOnAPristineStoreShowsNoPendingReset() throws {
        let runtime = try world.launch()
        let model = try productionModel(runtime)

        let refreshing = ResetUncheckedBox(model)
        try runtime.session.run { await refreshing.value.refresh() }

        XCTAssertEqual(model.phase, .idle)
        runtime.terminate()
    }

    // MARK: Optional credential reset

    private func runReset(_ model: ResetSettingsModel, on runtime: RealCoreResetRuntime) throws {
        let box = ResetUncheckedBox(model)
        try runtime.session.run { await box.value.confirmReset() }
    }

    func testKeepingCredentialsLeavesTheProviderProfileAndItsSecretInPlace() throws {
        let reference = try world.addProfile(version: "profile-keep")
        let runtime = try world.launch()
        let model = try productionModel(runtime)

        model.credentialChoice = .keep
        model.beginConfirmation()
        try runReset(model, on: runtime)

        XCTAssertEqual(model.phase, .completed)
        XCTAssertTrue(try world.isCredentialPresent(reference), "the secret was kept")
        XCTAssertFalse(try world.isProfileRevoked("profile-keep"), "the profile was kept")
        runtime.terminate()
    }

    func testRemovingCredentialsRevokesTheProfileAndDeletesItsSecretOnlyWhenChosen() throws {
        let reference = try world.addProfile(version: "profile-remove")
        let runtime = try world.launch()
        let model = try productionModel(runtime)

        model.credentialChoice = .remove
        model.beginConfirmation()
        try runReset(model, on: runtime)

        XCTAssertEqual(model.phase, .completed)
        XCTAssertFalse(try world.isCredentialPresent(reference), "the secret was removed")
        XCTAssertTrue(try world.isProfileRevoked("profile-remove"), "the profile was revoked")
        runtime.terminate()
    }

    func testAPendingRemovalIsFinishedByARetryUnderItsOriginalChoiceEvenIfKeepIsSelectedNow() throws {
        let reference = try world.addProfile(version: "profile-retry")
        let runtime = try world.launch(wrapCredentials: { FlakyCredentials($0, failures: 1) })
        let model = try productionModel(runtime)

        model.credentialChoice = .remove
        model.beginConfirmation()
        try runReset(model, on: runtime)

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("Some provider keys have not been removed yet."))
        XCTAssertTrue(try world.isCredentialPresent(reference), "the removal had not finished")

        model.credentialChoice = .keep
        let retrying = ResetUncheckedBox(model)
        try runtime.session.run { await retrying.value.retry() }

        XCTAssertEqual(model.phase, .completed)
        XCTAssertFalse(try world.isCredentialPresent(reference), "the original remove choice was carried out")
        XCTAssertEqual(try runtime.currentGeneration(), 1, "the retry started no further reset")
        runtime.terminate()
    }

    func testAfterARestartAnUnfinishedRemovalIsShownAndRetryFinishesIt() throws {
        let reference = try world.addProfile(version: "profile-restart")
        let first = try world.launch(wrapCredentials: { FlakyCredentials($0, failures: 1) })
        let firstModel = try productionModel(first)
        firstModel.credentialChoice = .remove
        firstModel.beginConfirmation()
        try runReset(firstModel, on: first)
        XCTAssertTrue(firstModel.phase.isPending)
        first.terminate()

        // A new process: the core says the generation completed, but the native removal is still owed.
        let second = try world.launch()
        let model = try productionModel(second)
        let refreshing = ResetUncheckedBox(model)
        try second.session.run { await refreshing.value.refresh() }

        XCTAssertEqual(model.phase, .pending(DeletionText.nativeWorkOwedText))
        XCTAssertTrue(model.canRetry)
        XCTAssertTrue(try world.isCredentialPresent(reference))

        try second.session.run { await refreshing.value.retry() }

        XCTAssertEqual(model.phase, .completed)
        XCTAssertFalse(try world.isCredentialPresent(reference))
        let again = ResetUncheckedBox(model)
        try second.session.run { await again.value.refresh() }
        XCTAssertEqual(model.phase, .completed, "nothing is owed any more")
        second.terminate()
    }
}
