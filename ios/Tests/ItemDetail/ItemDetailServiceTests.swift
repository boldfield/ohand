import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private func sampleDetail(itemID: String = "item-1") -> CoreItemDetail {
    CoreItemDetail(
        itemID: itemID, captureID: "capture-1", revision: 2, itemType: "note", scope: "personal",
        sessionTopic: "garage", lifecycleState: "active", capturedAt: "2026-10-08T09:30:00Z",
        text: "call the roofer tomorrow", textBasis: .corrected, originalText: "call the roofer",
        correctedText: "call the roofer tomorrow", audioReference: nil, transcript: nil,
        history: [
            CoreItemDetailEvent(
                eventID: "event-2", revision: 1, eventType: "correction", happenedAt: "2026-10-08T10:00:00Z",
                correction: CoreItemDetailCorrection(kind: "type", oldValue: nil, newValue: "note"))
        ],
        historyTruncated: false)
}

/// Answers each read with the next scripted result and records which identifiers the service asked about.
private final class ScriptedItemDetailCore: ItemDetailCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<CoreItemDetail, CoreFailure>]
    private var received: [String] = []

    init(_ script: [Result<CoreItemDetail, CoreFailure>]) {
        self.script = script
    }

    var requestedItemIDs: [String] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func read(itemID: String, completion: @escaping @Sendable (Result<CoreItemDetail, CoreFailure>) -> Void) {
        lock.lock()
        received.append(itemID)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreFailure.handleClosed))
    }
}

/// A read source that only records what the gate asked of the core.
private final class RecordingReadSource: PrivateReadSource {
    struct ItemDetailRequest {
        let operationID: UInt64
        let itemID: String
        let readScopes: [CoreExportScope]
    }

    private(set) var itemDetailRequests: [ItemDetailRequest] = []

    func startStoreCheck(operationID: UInt64) throws {}
    func startGetCapture(operationID: UInt64, captureID: String) throws {}
    func startItemStatus(operationID: UInt64, itemID: String) throws {}
    func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {}

    func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {
        itemDetailRequests.append(
            ItemDetailRequest(operationID: operationID, itemID: itemID, readScopes: readScopes))
    }
}

final class ItemDetailServiceTests: XCTestCase {
    private func failure(_ code: String, _ errorClass: CoreErrorClass = .permanent) -> CoreFailure {
        CoreFailure(errorClass: errorClass, code: code, message: "fixed message")
    }

    func testTheDetailComesBackAsTheCoreReportedIt() async throws {
        let core = ScriptedItemDetailCore([.success(sampleDetail())])
        let service = ItemDetailService(core: core)

        let detail = try await service.detail(itemID: "item-1")

        XCTAssertEqual(detail, sampleDetail())
        XCTAssertEqual(core.requestedItemIDs, ["item-1"])
    }

    func testAnEmptyOrOverlongIDIsRefusedWithoutAskingTheCore() async {
        let core = ScriptedItemDetailCore([])
        let service = ItemDetailService(core: core)

        for itemID in ["", String(repeating: "x", count: ItemDetailService.maximumItemIDBytes + 1),
            String(repeating: "é", count: ItemDetailService.maximumItemIDBytes / 2 + 1)]
        {
            do {
                _ = try await service.detail(itemID: itemID)
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ItemDetailError, .invalidItemID)
            }
        }
        XCTAssertEqual(core.requestedItemIDs, [], "a refused ID never reaches the core")
    }

    func testAnIDAtTheBoundAndAnOddlyShapedIDAreForwardedUnchanged() async throws {
        let core = ScriptedItemDetailCore([.success(sampleDetail()), .success(sampleDetail())])
        let service = ItemDetailService(core: core)
        let longest = String(repeating: "x", count: ItemDetailService.maximumItemIDBytes)
        let oddlyShaped = "../item 1;\n'--"

        _ = try await service.detail(itemID: longest)
        _ = try await service.detail(itemID: oddlyShaped)

        XCTAssertEqual(core.requestedItemIDs, [longest, oddlyShaped], "the ID is opaque: nothing is parsed or rewritten")
    }

    func testEachFailureMapsToTheContentFreeError() async {
        let cases: [(CoreFailure, ItemDetailError)] = [
            (failure("read_not_authenticated", .unauthorized), .notAuthenticated),
            (failure("read_relocked", .unauthorized), .relocked),
            (failure("not_found"), .notFound),
            (failure("invalid_request"), .invalidItemID),
            (failure("storage_error", .transient), .core(failure("storage_error", .transient))),
        ]
        for (coreFailure, expected) in cases {
            let service = ItemDetailService(core: ScriptedItemDetailCore([.failure(coreFailure)]))
            do {
                _ = try await service.detail(itemID: "item-1")
                XCTFail("expected \(expected)")
            } catch {
                XCTAssertEqual(error as? ItemDetailError, expected)
            }
        }
    }

    // MARK: the gated client

    private func makeGate(
        authenticated: Bool, scopes: [CoreExportScope] = [.personal, .work], source: RecordingReadSource
    ) -> (PrivateReadGate, ReadAuthenticationSession) {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return (PrivateReadGate(session: session, source: source, authorizedScopes: scopes), session)
    }

    private func readSynchronously(_ client: CoreItemDetailClient, itemID: String) -> [Result<CoreItemDetail, CoreFailure>] {
        let box = ResultsBox()
        client.read(itemID: itemID) { box.append($0) }
        return box.values
    }

    private final class ResultsBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [Result<CoreItemDetail, CoreFailure>] = []

        func append(_ value: Result<CoreItemDetail, CoreFailure>) {
            lock.lock()
            stored.append(value)
            lock.unlock()
        }

        var values: [Result<CoreItemDetail, CoreFailure>] {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }
    }

    private func detailEvent(_ operationID: UInt64, transcript: Any = NSNull()) throws -> CoreEvent {
        let payload: [String: Any] = [
            "operation_id": operationID, "item_id": "item-1", "capture_id": "capture-1", "revision": 0,
            "item_type": NSNull(), "scope": "personal", "session_topic": NSNull(), "lifecycle_state": "active",
            "captured_at": "2026-10-08T09:30:00Z", "text": "water the ferns", "text_basis": "original",
            "original_text": "water the ferns", "corrected_text": NSNull(), "audio_reference": NSNull(),
            "transcript": transcript, "history": [], "history_truncated": false, "suggestions_stopped": false,
        ]
        return CoreEvent(operationID: operationID, outcome: .success(try JSONSerialization.data(withJSONObject: payload)))
    }

    func testACaptureOnlySessionIsRefusedBeforeTheCoreIsAsked() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: false, source: source)
        let client = CoreItemDetailClient(gate: gate)

        let results = readSynchronously(client, itemID: "item-1")

        XCTAssertEqual(results.count, 1)
        if case .failure(let refusal)? = results.first {
            XCTAssertEqual(refusal.code, "read_not_authenticated")
        } else {
            XCTFail("expected a refusal")
        }
        XCTAssertTrue(source.itemDetailRequests.isEmpty)
    }

    func testTheGateAsksOnlyForTheSessionsAuthorizedScopesAndTheClientUsesItsOwnRange() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, scopes: [.personal], source: source)
        let client = CoreItemDetailClient(gate: gate)

        _ = readSynchronously(client, itemID: "item-1")
        _ = readSynchronously(client, itemID: "item-2")

        XCTAssertEqual(source.itemDetailRequests.map(\.readScopes), [[.personal], [.personal]])
        XCTAssertEqual(source.itemDetailRequests.map(\.itemID), ["item-1", "item-2"])
        XCTAssertEqual(
            source.itemDetailRequests.map(\.operationID),
            [CoreItemDetailClient.firstOperationID, CoreItemDetailClient.firstOperationID + 1])
    }

    func testAnAnswerWhileAuthenticatedIsDeliveredAndForeignEventsAreIgnored() throws {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, source: source)
        let client = CoreItemDetailClient(gate: gate)
        let box = ResultsBox()
        client.read(itemID: "item-1") { box.append($0) }
        let operationID = try XCTUnwrap(source.itemDetailRequests.first?.operationID)

        XCTAssertFalse(client.handle(CoreEvent(operationID: operationID + 1000, outcome: .success(Data()))))
        XCTAssertTrue(box.values.isEmpty)
        XCTAssertTrue(client.handle(try detailEvent(operationID)))

        guard case .success(let detail)? = box.values.first else { return XCTFail("expected the detail") }
        XCTAssertEqual(detail.text, "water the ferns")
        XCTAssertEqual(detail.textBasis, .original)
        XCTAssertFalse(client.handle(try detailEvent(operationID)), "an answer is delivered at most once")
    }

    func testTheTranscriptProvenanceCarriesTheJobAndTheExactAudioDigest() throws {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, source: source)
        let client = CoreItemDetailClient(gate: gate)
        let box = ResultsBox()
        client.read(itemID: "item-1") { box.append($0) }
        let operationID = try XCTUnwrap(source.itemDetailRequests.first?.operationID)
        let transcript: [String: Any] = [
            "transcript_record_id": "record-1", "source_job_id": "job-1", "text": "water the ferns tomorrow",
            "recognizer": "on-device-recognizer/1", "detected_language": "en-US", "confidence": 0.91,
            "transcribed_at": "2026-10-08T10:05:00Z", "attached_revision": 3,
            "audio_sha256": String(repeating: "ab", count: 32),
        ]

        XCTAssertTrue(client.handle(try detailEvent(operationID, transcript: transcript)))

        guard case .success(let detail)? = box.values.first else { return XCTFail("expected the detail") }
        XCTAssertEqual(
            detail.transcript,
            CoreItemDetailTranscript(
                transcriptRecordID: "record-1", sourceJobID: "job-1", text: "water the ferns tomorrow",
                recognizer: "on-device-recognizer/1", detectedLanguage: "en-US", confidence: 0.91,
                transcribedAt: "2026-10-08T10:05:00Z", attachedRevision: 3,
                audioSHA256: String(repeating: "ab", count: 32)))
    }

    func testAnAnswerThatArrivesAfterARelockIsDiscardedWhetherItIsASuccessOrAFailure() throws {
        let source = RecordingReadSource()
        let (gate, session) = makeGate(authenticated: true, source: source)
        let client = CoreItemDetailClient(gate: gate)
        let success = ResultsBox()
        let notFound = ResultsBox()
        client.read(itemID: "item-1") { success.append($0) }
        client.read(itemID: "item-2") { notFound.append($0) }
        let first = try XCTUnwrap(source.itemDetailRequests.first?.operationID)
        let second = try XCTUnwrap(source.itemDetailRequests.last?.operationID)

        session.relock(.enteredBackground)
        XCTAssertTrue(client.handle(try detailEvent(first)))
        XCTAssertTrue(
            client.handle(CoreEvent(operationID: second, outcome: .failure(failure("not_found")))))

        for box in [success, notFound] {
            guard case .failure(let refusal)? = box.values.first else { return XCTFail("expected a refusal") }
            XCTAssertEqual(refusal.code, "read_relocked")
            XCTAssertEqual(box.values.count, 1)
        }
    }

    func testShutDownFailsWaitingReadsAndRefusesLaterOnes() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, source: source)
        let client = CoreItemDetailClient(gate: gate)
        let waiting = ResultsBox()
        client.read(itemID: "item-1") { waiting.append($0) }

        client.shutDown()
        let later = readSynchronously(client, itemID: "item-2")

        for results in [waiting.values, later] {
            guard case .failure(let refusal)? = results.first else { return XCTFail("expected a failure") }
            XCTAssertEqual(refusal, CoreFailure.handleClosed)
        }
        XCTAssertEqual(source.itemDetailRequests.count, 1, "nothing is started after shut down")
    }
}
