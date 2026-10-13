import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Raw SQL is confined to this file, and only to rows no export creates: a provider profile with its credential
/// reference, and reads of the revocation column. Captures come from the real ingress import and the reset itself is the
/// real core operation; nothing here writes a reset generation, a tombstone or a cleanup row.
final class RealCoreResetWorld {
    let storage: RealCoreDeletionWorld
    let keychain = FakeKeychain()
    let credentials: CredentialService
    private var runtimes: [RealCoreResetRuntime] = []

    var layout: ProtectedStorageLayout { storage.layout }

    init() throws {
        storage = try RealCoreDeletionWorld()
        credentials = CredentialService(serviceName: "ohand.reset.tests", keychain: keychain)
    }

    func tearDown() {
        runtimes.forEach { $0.terminate() }
        runtimes = []
        storage.tearDown()
    }

    /// A process start over the on-disk store: a new core, clients, ingress service and reset service. `wrapStaging`
    /// puts a test decorator around the production staging, to act at a precise moment of the reset.
    func launch(wrapStaging: ((ResetIngressStaging) -> ResetIngressStaging)? = nil) throws -> RealCoreResetRuntime {
        let runtime = try RealCoreResetRuntime(world: self, wrapStaging: wrapStaging)
        runtimes.append(runtime)
        return runtime
    }

    // MARK: Seeding

    var captureContext: IngressCaptureContext {
        IngressCaptureContext(
            captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: "route-personal", entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z", sessionTopic: nil)
    }

    func textRecord(_ captureID: String) -> IngressRecord {
        IngressRecord(captureID: captureID, text: "buy oat milk", audio: nil, context: captureContext)
    }

    /// A capture routed to the work route, which `addWorkRoute` must have created before the core imports it.
    func workTextRecord(_ captureID: String) -> IngressRecord {
        var context = captureContext
        context.itemScope = "work"
        context.routeID = "route-work"
        return IngressRecord(captureID: captureID, text: "draft the synthetic status note", audio: nil, context: context)
    }

    /// A work-route text capture written to staging and never imported.
    func stageWorkTextRecord(captureID: String) throws {
        _ = try IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
            .stage(workTextRecord(captureID))
    }

    /// Creates the work route. No core may be open.
    func addWorkRoute() throws {
        try execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-work', 'route-work', 'work', '[]', '2026-10-08T09:00:00Z')")
    }

    func voiceRecord(_ captureID: String) -> IngressRecord {
        let handoff = IngressAudioHandoff(
            inProgressFileName: "\(captureID)-recording.m4a", finalizedFileName: "\(captureID).m4a")
        return IngressRecord(captureID: captureID, text: nil, audio: handoff, context: captureContext)
    }

    /// A text capture written to staging and never imported, as after a kill between staging and import.
    func stageTextRecord(captureID: String) throws {
        _ = try IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
            .stage(textRecord(captureID))
    }

    /// Adds a provider profile whose credential lives in the fake Keychain, and returns the credential reference.
    @discardableResult
    func addProfile(version: String) throws -> String {
        let reference = try credentials.addCredential(Data("synthetic secret for \(version)".utf8))
        try execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, "
                + "credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) "
                + "VALUES ('\(version)', 'profile-\(version)', 'openai', NULL, 'model-a', '\(reference)', 30, '{}', "
                + "'[]', '[]', '2026-10-10T09:00:00Z')")
        return reference
    }

    // MARK: Inspection

    func isCredentialPresent(_ reference: String) throws -> Bool {
        try credentials.credentialStatus(reference: reference) == .present
    }

    func isProfileRevoked(_ version: String) throws -> Bool {
        var revoked: Bool?
        try query("SELECT revoked_at IS NOT NULL FROM provider_profiles WHERE profile_version = '\(version)'") { statement in
            revoked = sqlite3_column_int(statement, 0) != 0
        }
        return try XCTUnwrap(revoked)
    }

    // MARK: SQLite

    private func execute(_ statement: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(storage.databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_close(database) }
        let status = sqlite3_exec(database, statement, nil, nil, nil)
        XCTAssertEqual(status, SQLITE_OK, "\(String(cString: sqlite3_errmsg(database))): \(statement)")
        if status != SQLITE_OK { throw StorageSetupFailed() }
    }

    private func query(_ sql: String, row: (OpaquePointer?) -> Void) throws {
        var database: OpaquePointer?
        guard sqlite3_open(storage.databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_close(database) }
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_finalize(statement) }
        while sqlite3_step(statement) == SQLITE_ROW { row(statement) }
    }
}

/// One "process": a real core over the on-disk store with the production deletion and reset clients and the ingress
/// importer attached to its single event handler. Events arrive on the main queue, where XCTest runs synchronous
/// tests, so waiting pumps the run loop. Closing it is a process end: nothing but the files survives.
final class RealCoreResetSession: ForegroundIngressImporting {
    let core: CoreHandle
    let deletionClient: CoreDeletionCleanupClient
    let resetClient: CoreResetClient
    private var pendingImports: [UInt64: (IngressImportResult) -> Void] = [:]
    private var nextImportOperationID: UInt64 = 1
    private var isClosed = false

    init(path: String) throws {
        core = try CoreHandle(path: path)
        deletionClient = CoreDeletionCleanupClient(core: core)
        resetClient = CoreResetClient(core: core)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            if self.deletionClient.handle(event) || self.resetClient.handle(event) { return }
            if let completion = self.pendingImports.removeValue(forKey: event.operationID) {
                completion(CoreIngressImporter.result(for: event))
            }
        }
    }

    func importRecord(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void) {
        let operationID = nextImportOperationID
        nextImportOperationID += 1
        pendingImports[operationID] = completion
        let capture = CaptureRecord(
            captureID: record.captureID,
            text: record.text,
            audioReference: record.coreAudioReference,
            captureInstant: record.context.captureInstant,
            timezoneID: record.context.timezoneID,
            utcOffsetMinutes: record.context.utcOffsetMinutes,
            locale: record.context.locale,
            calendar: record.context.calendar,
            itemScope: record.context.itemScope,
            routeID: record.context.routeID,
            entryLocked: record.context.entryLocked,
            createdAt: record.context.createdAt,
            sessionTopic: record.context.sessionTopic)
        do {
            try core.startImportForegroundIngress(operationID: operationID, record: capture)
        } catch {
            pendingImports[operationID] = nil
            completion(.failed(.coreUnavailable))
        }
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Runs one async call to completion while the main run loop delivers the core's events.
    func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = ResetAnswerBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    /// Imports a record the way a late callback or a replayed handoff would, and waits for the core's answer.
    func importAndWait(_ record: IngressRecord) -> IngressImportResult? {
        var result: IngressImportResult?
        importRecord(record) { result = $0 }
        _ = pump(until: { result != nil })
        return result
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        deletionClient.shutDown()
        resetClient.shutDown()
        core.close()
    }
}

/// A process with the production reset composition over the real core and the real protected-storage files.
final class RealCoreResetRuntime {
    let session: RealCoreResetSession
    let markers: FileResetMarkerStore
    private(set) var ingress: ForegroundIngressService?
    private(set) var service: ResetService?

    init(world: RealCoreResetWorld, wrapStaging: ((ResetIngressStaging) -> ResetIngressStaging)? = nil) throws {
        session = try RealCoreResetSession(path: world.storage.databasePath)
        markers = FileResetMarkerStore(layout: world.layout)
        let faults = world.storage.faults
        do {
            let gate = ResetIngressGate(wrapping: session, layout: world.layout)
            let ingress = try ForegroundIngressService(layout: world.layout, importer: gate)
            self.ingress = ingress
            let itemCleanup = DeletionCleanupService(
                core: FaultingDeletionCleanupCore(wrapped: session.deletionClient, faults: faults),
                notifications: FaultingNotificationCanceller(bridge: world.storage.makeBridge(), faults: faults),
                localCleanup: DeletionLocalCleanup(
                    layout: world.layout, fileSystem: FaultingDeletionFileSystem(faults: faults)))
            let nativeStaging = NativeResetIngress(service: ingress, gate: gate, layout: world.layout)
            service = ResetService(
                core: session.resetClient,
                staging: wrapStaging?(nativeStaging) ?? nativeStaging,
                itemCleanup: itemCleanup,
                exportTemporaries: ExportTemporaryCleaner(layout: world.layout),
                credentials: world.credentials,
                markers: markers)
        } catch {
            session.close()
            throw error
        }
    }

    func reset(_ choice: ResetCredentialChoice) throws -> ResetReport {
        let service = try XCTUnwrap(service)
        return try session.run { await service.reset(credentialChoice: choice) }
    }

    func resume() throws -> ResetReport {
        let service = try XCTUnwrap(service)
        return try session.run { await service.resume() }
    }

    /// Submits a capture through the foreground ingress path on the main queue without waiting for its outcome. Safe to
    /// call from the reset's own task, where the outcome cannot arrive until the reset releases the ingress boundary.
    func submitLater(_ record: IngressRecord, outcome: @escaping (IngressOutcome) -> Void) {
        DispatchQueue.main.async { [weak self] in
            self?.ingress?.submit(record, completion: outcome)
        }
    }

    /// Submits a new capture through the foreground ingress path and returns the item the core created.
    func capture(_ record: IngressRecord) throws -> String {
        let ingress = try XCTUnwrap(self.ingress)
        var outcome: IngressOutcome?
        ingress.submit(record) { outcome = $0 }
        XCTAssertTrue(session.pump(until: { outcome != nil }))
        guard case .saved(let acknowledgment)? = outcome else {
            XCTFail("the capture was not saved: \(String(describing: outcome))")
            throw StorageSetupFailed()
        }
        return acknowledgment.itemID
    }

    func generation(ofItem itemID: String) throws -> Int64? {
        let client = session.resetClient
        return try session.run { try await client.generation(ofItem: itemID) }
    }

    func currentGeneration() throws -> Int64 {
        let client = session.resetClient
        return try session.run { try await client.currentGeneration() }
    }

    /// Ends the process: the ingress service (and its writer lock) goes away, then the core closes.
    func terminate() {
        service = nil
        ingress = nil
        session.close()
    }
}

private final class ResetAnswerBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// The production staging, with a hook that runs right after the snapshot: the moment a capture can land after the tap.
final class SnapshotHookStaging: ResetIngressStaging {
    private let wrapped: ResetIngressStaging
    private let afterSnapshot: () -> Void

    init(_ wrapped: ResetIngressStaging, afterSnapshot: @escaping () -> Void) {
        self.wrapped = wrapped
        self.afterSnapshot = afterSnapshot
    }

    func snapshot() async throws -> ResetStagingSnapshot {
        let taken = try await wrapped.snapshot()
        afterSnapshot()
        return taken
    }

    func endBoundary() { wrapped.endBoundary() }

    func importSnapshot(_ snapshot: ResetStagingSnapshot) async { await wrapped.importSnapshot(snapshot) }

    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge {
        await wrapped.purgeUnimported(from: snapshot)
    }
}
