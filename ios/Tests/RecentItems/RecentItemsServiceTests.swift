import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private func samplePage() -> CoreRecentItemsPage {
    CoreRecentItemsPage(
        items: [
            CoreRecentItem(
                itemID: "item-2", revision: 1, itemType: nil, displayText: "water the ferns",
                capturedAt: "2026-10-08T09:30:00Z", sourceKind: .text),
            CoreRecentItem(
                itemID: "item-1", revision: 3, itemType: "note", displayText: nil,
                capturedAt: "2026-10-07T09:30:00Z", sourceKind: .voice),
        ],
        nextCursor: "2026-10-07T09:30:00Z|item-1")
}

/// Answers each read with the next scripted result and records the page requests the service made.
private final class ScriptedRecentItemsCore: RecentItemsCore, @unchecked Sendable {
    struct Request: Equatable {
        let limit: Int?
        let cursor: String?
    }

    private let lock = NSLock()
    private var script: [Result<CoreRecentItemsPage, CoreFailure>]
    private var received: [Request] = []

    init(_ script: [Result<CoreRecentItemsPage, CoreFailure>]) {
        self.script = script
    }

    var requests: [Request] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func read(
        limit: Int?, cursor: String?,
        completion: @escaping @Sendable (Result<CoreRecentItemsPage, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(Request(limit: limit, cursor: cursor))
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreFailure.handleClosed))
    }
}

/// A read source that only records what the gate asked of the core.
private final class RecordingReadSource: PrivateReadSource {
    struct RecentItemsRequest {
        let operationID: UInt64
        let readScopes: [CoreExportScope]
        let limit: Int?
        let cursor: String?
    }

    private(set) var recentItemsRequests: [RecentItemsRequest] = []

    func startStoreCheck(operationID: UInt64) throws {}
    func startGetCapture(operationID: UInt64, captureID: String) throws {}
    func startItemStatus(operationID: UInt64, itemID: String) throws {}
    func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {}
    func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {}

    func startRecentItems(operationID: UInt64, readScopes: [CoreExportScope], limit: Int?, cursor: String?) throws {
        recentItemsRequests.append(
            RecentItemsRequest(operationID: operationID, readScopes: readScopes, limit: limit, cursor: cursor))
    }
}

final class RecentItemsServiceTests: XCTestCase {
    private func failure(_ code: String, _ errorClass: CoreErrorClass = .permanent) -> CoreFailure {
        CoreFailure(errorClass: errorClass, code: code, message: "fixed message")
    }

    func testThePageComesBackAsTheCoreReportedItAndTheCursorIsForwarded() async throws {
        let core = ScriptedRecentItemsCore([.success(samplePage())])
        let service = RecentItemsService(core: core)

        let page = try await service.page(limit: 2, cursor: "2026-10-09T09:30:00Z|item-3")

        XCTAssertEqual(page, samplePage())
        XCTAssertEqual(core.requests, [.init(limit: 2, cursor: "2026-10-09T09:30:00Z|item-3")])
    }

    func testAPageSizeOutsideTheCoreBoundsIsRefusedWithoutAskingTheCore() async {
        let core = ScriptedRecentItemsCore([])
        let service = RecentItemsService(core: core)

        for limit in [0, -1, RecentItemsService.maximumLimit + 1] {
            do {
                _ = try await service.page(limit: limit)
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? RecentItemsError, .invalidRequest)
            }
        }
        XCTAssertEqual(core.requests, [])
    }

    func testEachFailureMapsToTheContentFreeError() async {
        let cases: [(CoreFailure, RecentItemsError)] = [
            (failure("read_not_authenticated", .unauthorized), .notAuthenticated),
            (failure("read_relocked", .unauthorized), .relocked),
            (failure("invalid_request"), .invalidRequest),
            (failure("storage_error", .transient), .core(failure("storage_error", .transient))),
        ]
        for (coreFailure, expected) in cases {
            let service = RecentItemsService(core: ScriptedRecentItemsCore([.failure(coreFailure)]))
            do {
                _ = try await service.page()
                XCTFail("expected \(expected)")
            } catch {
                XCTAssertEqual(error as? RecentItemsError, expected)
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

    private final class ResultsBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [Result<CoreRecentItemsPage, CoreFailure>] = []

        func append(_ value: Result<CoreRecentItemsPage, CoreFailure>) {
            lock.lock()
            stored.append(value)
            lock.unlock()
        }

        var values: [Result<CoreRecentItemsPage, CoreFailure>] {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }
    }

    private func pageEvent(_ operationID: UInt64) throws -> CoreEvent {
        let payload: [String: Any] = [
            "operation_id": operationID,
            "items": [
                [
                    "item_id": "item-1", "revision": 0, "item_type": NSNull(), "display_text": "water the ferns",
                    "captured_at": "2026-10-08T09:30:00Z", "source_kind": "text",
                ]
            ],
            "next_cursor": NSNull(),
        ]
        return CoreEvent(operationID: operationID, outcome: .success(try JSONSerialization.data(withJSONObject: payload)))
    }

    func testACaptureOnlySessionIsRefusedBeforeTheCoreIsAsked() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: false, source: source)
        let client = CoreRecentItemsClient(gate: gate)
        let box = ResultsBox()

        client.read(limit: nil, cursor: nil) { box.append($0) }

        XCTAssertEqual(box.values.count, 1)
        if case .failure(let refusal)? = box.values.first {
            XCTAssertEqual(refusal.code, "read_not_authenticated")
        } else {
            XCTFail("expected a refusal")
        }
        XCTAssertTrue(source.recentItemsRequests.isEmpty)
    }

    func testTheGateAsksOnlyForTheSessionsAuthorizedScopesAndTheClientUsesItsOwnRange() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, scopes: [.personal], source: source)
        let client = CoreRecentItemsClient(gate: gate)

        client.read(limit: 10, cursor: nil) { _ in }
        client.read(limit: nil, cursor: "a|b") { _ in }

        XCTAssertEqual(source.recentItemsRequests.map(\.readScopes), [[.personal], [.personal]])
        XCTAssertEqual(source.recentItemsRequests.map(\.limit), [10, nil])
        XCTAssertEqual(source.recentItemsRequests.map(\.cursor), [nil, "a|b"])
        XCTAssertEqual(
            source.recentItemsRequests.map(\.operationID),
            [CoreRecentItemsClient.firstOperationID, CoreRecentItemsClient.firstOperationID + 1])
    }

    func testAnAnswerWhileAuthenticatedIsDeliveredOnceAndForeignEventsAreIgnored() throws {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, source: source)
        let client = CoreRecentItemsClient(gate: gate)
        let box = ResultsBox()
        client.read(limit: nil, cursor: nil) { box.append($0) }
        let operationID = try XCTUnwrap(source.recentItemsRequests.first?.operationID)

        XCTAssertFalse(client.handle(CoreEvent(operationID: operationID + 1000, outcome: .success(Data()))))
        XCTAssertTrue(box.values.isEmpty)
        XCTAssertTrue(client.handle(try pageEvent(operationID)))

        guard case .success(let page)? = box.values.first else { return XCTFail("expected the page") }
        XCTAssertEqual(page.items.map(\.itemID), ["item-1"])
        XCTAssertEqual(page.items.first?.displayText, "water the ferns")
        XCTAssertEqual(page.items.first?.sourceKind, .text)
        XCTAssertNil(page.items.first?.itemType)
        XCTAssertNil(page.nextCursor)
        XCTAssertFalse(client.handle(try pageEvent(operationID)), "an answer is delivered at most once")
    }

    func testAnAnswerThatArrivesAfterARelockIsDiscardedWhetherItIsASuccessOrAFailure() throws {
        let source = RecordingReadSource()
        let (gate, session) = makeGate(authenticated: true, source: source)
        let client = CoreRecentItemsClient(gate: gate)
        let success = ResultsBox()
        let failed = ResultsBox()
        client.read(limit: nil, cursor: nil) { success.append($0) }
        client.read(limit: nil, cursor: nil) { failed.append($0) }
        let first = try XCTUnwrap(source.recentItemsRequests.first?.operationID)
        let second = try XCTUnwrap(source.recentItemsRequests.last?.operationID)

        session.relock(.enteredBackground)
        XCTAssertTrue(client.handle(try pageEvent(first)))
        XCTAssertTrue(
            client.handle(CoreEvent(operationID: second, outcome: .failure(failure("storage_error", .transient)))))

        for box in [success, failed] {
            guard case .failure(let refusal)? = box.values.first else { return XCTFail("expected a refusal") }
            XCTAssertEqual(refusal.code, "read_relocked")
            XCTAssertEqual(box.values.count, 1)
        }
    }

    func testShutDownFailsWaitingReadsAndRefusesLaterOnes() {
        let source = RecordingReadSource()
        let (gate, _) = makeGate(authenticated: true, source: source)
        let client = CoreRecentItemsClient(gate: gate)
        let waiting = ResultsBox()
        let later = ResultsBox()
        client.read(limit: nil, cursor: nil) { waiting.append($0) }

        client.shutDown()
        client.read(limit: nil, cursor: nil) { later.append($0) }

        for box in [waiting, later] {
            guard case .failure(let refusal)? = box.values.first else { return XCTFail("expected a failure") }
            XCTAssertEqual(refusal, CoreFailure.handleClosed)
        }
        XCTAssertEqual(source.recentItemsRequests.count, 1, "nothing is started after shut down")
    }

    func testTheRecentItemsRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.recentItemsClientName })
        XCTAssertEqual(mine.start, CoreRecentItemsClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }
}
