import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each command with the next scripted result and records what the service asked, in order.
private final class ScriptedItemDeletionCore: ItemDeletionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ItemDeletionAnswer, CoreFailure>]
    private var received: [ItemDeletionCommand] = []

    init(_ script: [Result<ItemDeletionAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [ItemDeletionCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: ItemDeletionCommand,
        completion: @escaping @Sendable (Result<ItemDeletionAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreItemDeletionClient.unreadableAnswer))
    }
}

/// Records the items it was asked to clean up and the core commands that had been sent by then.
private final class RecordingCleanup: ItemDeletionCleanup, @unchecked Sendable {
    private let lock = NSLock()
    private let result: DeletionItemResult
    private let core: ScriptedItemDeletionCore
    private var cleaned: [String] = []
    private var commandsSeen: [[ItemDeletionCommand]] = []

    init(result: DeletionItemResult, core: ScriptedItemDeletionCore) {
        self.result = result
        self.core = core
    }

    var items: [String] {
        lock.lock()
        defer { lock.unlock() }
        return cleaned
    }

    var commandsWhenAsked: [[ItemDeletionCommand]] {
        lock.lock()
        defer { lock.unlock() }
        return commandsSeen
    }

    func cleanUp(itemID: String) async -> DeletionItemResult {
        let seen = core.commands
        lock.lock()
        cleaned.append(itemID)
        commandsSeen.append(seen)
        lock.unlock()
        return result
    }
}

final class ItemDeletionServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func recorded(
        replayed: Bool = false, progress: ItemDeletionProgressPayload? = nil
    ) -> ItemDeletionAnswer {
        ItemDeletionAnswer(
            outcome: "deletion_recorded", itemID: "item-1", deletionRevision: 2, replayed: replayed,
            progress: progress
                ?? ItemDeletionProgressPayload(
                    state: "in_progress", pending: ["cancel_notifications", "clear_ingress", "remove_audio"]))
    }

    private func progress(_ payload: ItemDeletionProgressPayload) -> ItemDeletionAnswer {
        ItemDeletionAnswer(outcome: "progress", itemID: "item-1", progress: payload)
    }

    private let complete = ItemDeletionProgressPayload(state: "complete")

    private func settled() -> DeletionItemResult {
        DeletionItemResult(itemID: "item-1", listingFailure: nil, steps: [])
    }

    private func unsettled() -> DeletionItemResult {
        DeletionItemResult(itemID: "item-1", listingFailure: .coreUnavailable, steps: [])
    }

    private func makeService(
        _ script: [Result<ItemDeletionAnswer, CoreFailure>], authenticated: Bool = true,
        cleanup: DeletionItemResult? = nil
    ) -> (ItemDeletionService, ScriptedItemDeletionCore, RecordingCleanup?) {
        let core = ScriptedItemDeletionCore(script)
        let service = ItemDeletionService(core: core, session: makeSession(authenticated: authenticated))
        var recording: RecordingCleanup?
        if let cleanup {
            let created = RecordingCleanup(result: cleanup, core: core)
            recording = created
            service.setCleanupProvider { created }
        }
        return (service, core, recording)
    }

    func testDeletionRecordsTheIntentThenTriggersCleanupAndReportsTheProgressAfterIt() async throws {
        let (service, core, cleanup) = makeService(
            [.success(recorded()), .success(progress(complete))], cleanup: settled())

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(
            receipt,
            ItemDeletionReceipt(
                itemID: "item-1", deletionRevision: 2, replayed: false, progress: .complete, cleanupSettled: true))
        XCTAssertEqual(
            core.commands,
            [.markDeleted(itemID: "item-1", expectedRevision: 1), .progress(itemID: "item-1")])
        XCTAssertEqual(cleanup?.items, ["item-1"])
        XCTAssertEqual(
            cleanup?.commandsWhenAsked, [[.markDeleted(itemID: "item-1", expectedRevision: 1)]],
            "cleanup is triggered only after the intent is confirmed")
    }

    func testAFailedCleanupLeavesTheIntentAndReportsTheRemainingWork() async throws {
        let remaining = ItemDeletionProgressPayload(state: "in_progress", pending: ["remove_audio"])
        let (service, _, cleanup) = makeService(
            [.success(recorded()), .success(progress(remaining))], cleanup: unsettled())

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(receipt.progress, .inProgress(pending: [.removeAudio], failed: []))
        XCTAssertFalse(receipt.cleanupSettled)
        XCTAssertEqual(cleanup?.items, ["item-1"])
    }

    func testWithoutACleanupServiceTheIntentStandsAndIsReportedUnsettled() async throws {
        let (service, core, _) = makeService([.success(recorded()), .success(progress(.init(state: "in_progress")))])

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(receipt.deletionRevision, 2)
        XCTAssertFalse(receipt.cleanupSettled)
        XCTAssertEqual(core.commands.count, 2)
    }

    func testAProgressReadFailureAfterTheIntentFallsBackToTheIntentsProgress() async throws {
        let (service, _, _) = makeService(
            [.success(recorded()), .failure(CoreFailure(errorClass: .transient, code: "storage_error", message: "x"))],
            cleanup: settled())

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(
            receipt.progress,
            .inProgress(pending: [.cancelNotifications, .clearIngress, .removeAudio], failed: []))
        XCTAssertFalse(receipt.cleanupSettled)
    }

    func testAProgressReadRefusedByTheGateAfterTheIntentReportsOnlyTheIntentsOwnProgress() async throws {
        let relocked = CoreFailure(
            errorClass: .unauthorized, code: "read_relocked", message: "the session relocked before the answer")
        let (service, core, cleanup) = makeService(
            [.success(recorded()), .failure(relocked)], cleanup: settled())

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(receipt.deletionRevision, 2, "the confirmed deletion is still reported")
        XCTAssertEqual(
            receipt.progress,
            .inProgress(pending: [.cancelNotifications, .clearIngress, .removeAudio], failed: []),
            "the receipt carries the deletion answer's progress, not a fresh read")
        XCTAssertFalse(receipt.cleanupSettled)
        XCTAssertEqual(cleanup?.items, ["item-1"])
        XCTAssertEqual(core.commands.count, 2)
    }

    func testGateRefusalsOfAProgressReadAreReported() async {
        let (service, _, _) = makeService([
            .failure(CoreFailure(errorClass: .unauthorized, code: "read_relocked", message: "x")),
            .failure(CoreFailure(errorClass: .unauthorized, code: "read_not_authenticated", message: "x")),
        ])
        for expected in [ItemDeletionError.relocked, .notAuthenticated] {
            do {
                _ = try await service.progress(itemID: "item-1")
                XCTFail("a refused read delivers nothing")
            } catch {
                XCTAssertEqual(error as? ItemDeletionError, expected)
            }
        }
    }

    func testARepeatedDeletionReportsAReplayAndRetriesCleanup() async throws {
        let (service, _, cleanup) = makeService(
            [.success(recorded(replayed: true)), .success(progress(complete))], cleanup: settled())

        let receipt = try await service.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertTrue(receipt.replayed)
        XCTAssertEqual(cleanup?.items, ["item-1"])
    }

    func testAStaleRevisionIsRefusedAndNoCleanupRuns() async {
        let (service, core, cleanup) = makeService(
            [.success(ItemDeletionAnswer(outcome: "stale_revision", currentRevision: 5))], cleanup: settled())
        do {
            _ = try await service.delete(itemID: "item-1", expectedRevision: 1)
            XCTFail("a stale revision must not delete")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .staleRevision(currentRevision: 5))
        }
        XCTAssertEqual(core.commands.count, 1)
        XCTAssertEqual(cleanup?.items, [])
    }

    func testCaptureOnlySessionIsRefusedAndNeitherTheCoreNorCleanupIsAsked() async {
        let (service, core, cleanup) = makeService(
            [.success(recorded())], authenticated: false, cleanup: settled())
        do {
            _ = try await service.delete(itemID: "item-1", expectedRevision: 1)
            XCTFail("a capture-only session must not delete")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .notAuthenticated)
        }
        do {
            _ = try await service.progress(itemID: "item-1")
            XCTFail("a capture-only session must not read progress")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
        XCTAssertEqual(cleanup?.items, [])
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let (service, core, _) = makeService([.success(recorded())])
        for (itemID, revision) in [("", 0), ("item-1", -1)] {
            do {
                _ = try await service.delete(itemID: itemID, expectedRevision: revision)
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ItemDeletionError, .invalidRequest)
            }
        }
        do {
            _ = try await service.progress(itemID: "")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .invalidRequest)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testAnUnknownItemIsNotFoundAndCoreFailuresAreWrapped() async {
        let notFound = CoreFailure(errorClass: .permanent, code: "not_found", message: "x")
        let broken = CoreFailure(errorClass: .transient, code: "storage_error", message: "x")
        let (service, _, cleanup) = makeService([.failure(notFound), .failure(broken)], cleanup: settled())
        do {
            _ = try await service.delete(itemID: "item-1", expectedRevision: 1)
            XCTFail("expected not found")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .itemNotFound)
        }
        do {
            _ = try await service.delete(itemID: "item-1", expectedRevision: 1)
            XCTFail("expected a core failure")
        } catch {
            XCTAssertEqual(error as? ItemDeletionError, .core(broken))
        }
        XCTAssertEqual(cleanup?.items, [], "an intent the core did not confirm triggers no cleanup")
    }

    func testProgressMapsEveryState() async throws {
        let (service, core, _) = makeService([
            .success(progress(.init(state: "not_requested"))),
            .success(progress(.init(state: "in_progress", pending: ["clear_ingress"], failed: ["remove_audio"]))),
            .success(progress(.init(state: "failed", failed: ["cancel_notifications"]))),
            .success(progress(complete)),
            .success(progress(.init(state: "mystery"))),
            .success(progress(.init(state: "in_progress", pending: ["unknown_effect"]))),
        ])

        let first = try await service.progress(itemID: "item-1")
        let second = try await service.progress(itemID: "item-1")
        let third = try await service.progress(itemID: "item-1")
        let fourth = try await service.progress(itemID: "item-1")
        XCTAssertEqual(first, .notRequested)
        XCTAssertEqual(second, .inProgress(pending: [.clearIngress], failed: [.removeAudio]))
        XCTAssertEqual(third, .failed([.cancelNotifications]))
        XCTAssertEqual(fourth, .complete)
        for _ in 0..<2 {
            do {
                _ = try await service.progress(itemID: "item-1")
                XCTFail("an unknown state or effect is unreadable")
            } catch {
                XCTAssertEqual(error as? ItemDeletionError, .unreadable)
            }
        }
        XCTAssertEqual(core.commands, Array(repeating: .progress(itemID: "item-1"), count: 6))
    }
}
