import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Drives `ResetSettingsModel` over the production `ResetService`, the production core clients and the real Rust core on
/// a file store, using the reset fixture's real protected-storage files. Only the read-session wrapper is replaced by
/// the model's own scope closure; every reset step, generation and tombstone is the core's. Everything is synthetic.
@MainActor
final class ResetSettingsRealCoreTests: XCTestCase {
    /// `ResetControl` without its read session: the model has already checked the scope before it calls.
    private final class ServiceControls: ResetControlling, @unchecked Sendable {
        let service: ResetService
        let core: ResetCore

        init(service: ResetService, core: ResetCore) {
            self.service = service
            self.core = core
        }

        func reset(credentialChoice: ResetCredentialChoice) async throws -> ResetOutcome {
            let report = await service.reset(credentialChoice: credentialChoice)
            return ResetOutcome(state: report.state, progress: report.progress, blockers: report.blockers)
        }

        func progress() async throws -> ResetProgress? {
            try await core.status()
        }
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

    func testAConfirmedResetCompletesAndCaptureResumesUnderTheNewGeneration() async throws {
        let oldItem = try world.storage.importVoiceCapture(captureID: "cap-old")
        let runtime = try world.launch()
        let service = try XCTUnwrap(runtime.service)
        let model = ResetSettingsModel(
            controls: ServiceControls(service: service, core: runtime.session.resetClient), readScope: { .authenticated })
        XCTAssertEqual(try runtime.generation(ofItem: oldItem), 0)

        model.credentialChoice = .keep
        model.beginConfirmation()
        await model.confirmReset()

        XCTAssertEqual(model.phase, .completed)
        XCTAssertEqual(world.storage.remnants(of: "cap-old"), [], "the earlier capture's files are gone")
        XCTAssertEqual(try runtime.currentGeneration(), 1)

        let newItem = try runtime.capture(world.textRecord("cap-new"))
        XCTAssertEqual(try runtime.generation(ofItem: newItem), 1, "capture resumed under the new generation")
        runtime.terminate()
    }

    func testAFreshModelOnAPristineStoreShowsNoPendingReset() async throws {
        let runtime = try world.launch()
        let service = try XCTUnwrap(runtime.service)
        let model = ResetSettingsModel(
            controls: ServiceControls(service: service, core: runtime.session.resetClient), readScope: { .authenticated })

        await model.refresh()

        XCTAssertEqual(model.phase, .idle)
        runtime.terminate()
    }
}
