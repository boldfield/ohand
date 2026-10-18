import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Drives `ExportSettingsModel` over the production `ExportService`, the production `CoreExportClient` and the real Rust
/// core on an on-disk store, with real protected-storage files. Everything is synthetic. The work route and the provider
/// profile row (a sentinel endpoint and credential reference that must never appear in an export) are seeded with SQL
/// because the core has no native entry point for either yet.
@MainActor
final class ExportSettingsRealCoreTests: XCTestCase {
    private static let personalText = "synthetic personal note"
    private static let workText = "synthetic work note"
    private static let endpointSentinel = "https://private-endpoint.invalid/v1"
    private static let credentialSentinel = "keychain:credential-sentinel"

    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreDeletionWorld!
    private var session: RealCoreExportSession!
    private var readSession: ReadAuthenticationSession!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreDeletionWorld()
        try Self.execute(
            world.databasePath,
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-work', 'route-work', 'work', '[]', '2026-10-08T09:00:00Z')")
        try Self.execute(
            world.databasePath,
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, credential_ref, "
                + "timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) "
                + "VALUES ('profile-v1', 'profile', 'self_hosted', '\(Self.endpointSentinel)', 'model-x', "
                + "'\(Self.credentialSentinel)', 30, '{}', '[]', '[]', '2026-10-08T09:00:00Z')")
        _ = try world.importVoiceCapture(captureID: "cap-voice")
        let importing = try RealCoreExportSession(path: world.databasePath)
        _ = try importing.importCapture(textCapture("note-1", Self.personalText, scope: "personal"))
        _ = try importing.importCapture(textCapture("work-1", Self.workText, scope: "work"))
        importing.close()
        session = try RealCoreExportSession(path: world.databasePath)
        readSession = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        readSession.authenticate(reason: "test") { _ in }
        XCTAssertEqual(readSession.scope, .authenticated)
    }

    override func tearDown() {
        session?.close()
        session = nil
        world?.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func textCapture(_ captureID: String, _ text: String, scope: String) -> CaptureRecord {
        CaptureRecord(
            captureID: captureID, text: text, captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC",
            utcOffsetMinutes: 0, locale: "en_US", calendar: "gregorian", itemScope: scope, routeID: "route-\(scope)",
            entryLocked: false, createdAt: "2026-10-08T09:31:00Z", sessionTopic: nil)
    }

    private func makeModel() -> ExportSettingsModel {
        let service = ExportService(core: session.client, session: readSession, layout: world.layout)
        let read = readSession!
        return ExportSettingsModel(
            controls: service, readScope: { read.scope == .authenticated ? .authenticated : .captureOnly })
    }

    private func exportRemnants() -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: world.layout.rootDirectory.path)) ?? []
        return subpaths.filter { $0.hasPrefix("Temporary/" + ExportService.exportsDirectoryName + "/") }.sorted()
    }

    private static func execute(_ path: String, _ statement: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(path, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5_000)
        guard sqlite3_exec(database, statement, nil, nil, nil) == SQLITE_OK else { throw StorageSetupFailed() }
    }

    func testOnlyTheSelectedScopeIsExportedAndNoSecretIsIncluded() async throws {
        let model = makeModel()
        model.setScope(.personal, selected: true)

        await model.startExport()

        XCTAssertEqual(model.phase, .sharing)
        let prepared = try XCTUnwrap(model.pendingShare)
        let text = try String(contentsOf: prepared.exportFile, encoding: .utf8)
        XCTAssertTrue(text.contains(Self.personalText))
        for forbidden in [Self.workText, Self.endpointSentinel, Self.credentialSentinel, "FinalizedAudio/"] {
            XCTAssertFalse(text.contains(forbidden), "the export must not contain \(forbidden)")
        }
        XCTAssertEqual(prepared.shareItems, [prepared.exportFile], "audio stays off unless turned on")
        model.clear()
        XCTAssertEqual(exportRemnants(), [])
    }

    func testTheAudioChoiceReachesTheCoreAndACancelledShareRemovesCopiedAudioButNotTheSource() async throws {
        let model = makeModel()
        model.setScope(.personal, selected: true)
        model.includeAudio = true

        await model.startExport()

        let prepared = try XCTUnwrap(model.pendingShare)
        XCTAssertEqual(prepared.audio.includedCaptureIDs, ["cap-voice"])
        XCTAssertEqual(prepared.audioFiles.map { $0.lastPathComponent }, ["cap-voice.m4a"])
        XCTAssertFalse(exportRemnants().isEmpty)

        model.shareEnded(.cancelled)

        guard case .finished(.cancelled(let shown)) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(shown.contains("Nothing was sent"))
        XCTAssertEqual(exportRemnants(), [], "the export file and the copied audio are gone")
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-voice.m4a"), "the source audio is untouched")
        XCTAssertFalse(shown.contains("cap-voice"), "no capture identifier is shown")
    }

    func testBothScopesAreExportedAndACompletedShareCleansUp() async throws {
        let model = makeModel()
        model.setScope(.personal, selected: true)
        model.setScope(.work, selected: true)

        await model.startExport()
        let prepared = try XCTUnwrap(model.pendingShare)
        let text = try String(contentsOf: prepared.exportFile, encoding: .utf8)
        XCTAssertTrue(text.contains(Self.personalText) && text.contains(Self.workText))
        model.shareEnded(.completed)

        guard case .finished(.shared) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertEqual(exportRemnants(), [])
    }

    func testARelockedSessionIsRefusedBeforeAnythingIsRead() async throws {
        let model = makeModel()
        model.setScope(.personal, selected: true)
        readSession.relock(.explicit)

        await model.startExport()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(exportRemnants(), [])
    }
}
