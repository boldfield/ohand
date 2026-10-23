import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each command with the next scripted result and records what the service asked.
private final class ScriptedItemLifecycleControlCore: ItemLifecycleControlCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ItemLifecycleControlAnswer, CoreFailure>]
    private var received: [ItemLifecycleControlCommand] = []

    init(_ script: [Result<ItemLifecycleControlAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [ItemLifecycleControlCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: ItemLifecycleControlCommand,
        completion: @escaping @Sendable (Result<ItemLifecycleControlAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreItemLifecycleControlClient.unreadableAnswer))
    }
}

final class ItemLifecycleControlServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func applied(revision: Int = 3, replayed: Bool = false) -> ItemLifecycleControlAnswer {
        ItemLifecycleControlAnswer(outcome: "applied", itemID: "item-1", revision: revision, replayed: replayed)
    }

    func testEachControlSendsItsCommandAndReturnsTheReceipt() async throws {
        let core = ScriptedItemLifecycleControlCore([
            .success(applied(revision: 3)), .success(applied(revision: 4)), .success(applied(revision: 5)),
            .success(applied(revision: 6, replayed: true)),
        ])
        let service = ItemLifecycleControlService(core: core, session: makeSession(authenticated: true))

        let completed = try await service.complete(commandID: "c-1", itemID: "item-1", expectedRevision: 2)
        let cancelled = try await service.cancel(commandID: "c-2", itemID: "item-1", expectedRevision: 3)
        let snoozed = try await service.notNow(commandID: "c-3", itemID: "item-1", expectedRevision: 4)
        let muted = try await service.stopSuggesting(commandID: "c-4", itemID: "item-1", expectedRevision: 5)

        XCTAssertEqual(completed, ItemLifecycleControlReceipt(itemID: "item-1", revision: 3, replayed: false))
        XCTAssertEqual(cancelled.revision, 4)
        XCTAssertEqual(snoozed.revision, 5)
        XCTAssertTrue(muted.replayed)
        XCTAssertEqual(
            core.commands,
            [
                ItemLifecycleControlCommand(commandID: "c-1", itemID: "item-1", expectedRevision: 2, control: .complete),
                ItemLifecycleControlCommand(commandID: "c-2", itemID: "item-1", expectedRevision: 3, control: .cancel),
                ItemLifecycleControlCommand(commandID: "c-3", itemID: "item-1", expectedRevision: 4, control: .notNow),
                ItemLifecycleControlCommand(
                    commandID: "c-4", itemID: "item-1", expectedRevision: 5, control: .stopSuggesting),
            ])
    }

    func testCaptureOnlySessionIsRefusedAndTheCoreIsNotAsked() async {
        let core = ScriptedItemLifecycleControlCore([.success(applied())])
        let service = ItemLifecycleControlService(core: core, session: makeSession(authenticated: false))
        do {
            _ = try await service.complete(commandID: "c-1", itemID: "item-1", expectedRevision: 0)
            XCTFail("a capture-only session must not complete an item")
        } catch {
            XCTAssertEqual(error as? ItemLifecycleControlError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testRelockedSessionIsRefusedAgain() async {
        let core = ScriptedItemLifecycleControlCore([.success(applied())])
        let session = makeSession(authenticated: true)
        let service = ItemLifecycleControlService(core: core, session: session)
        session.relock(.explicit)
        do {
            _ = try await service.stopSuggesting(commandID: "c-1", itemID: "item-1", expectedRevision: 0)
            XCTFail("a relocked session must not quiet an item")
        } catch {
            XCTAssertEqual(error as? ItemLifecycleControlError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let core = ScriptedItemLifecycleControlCore([])
        let service = ItemLifecycleControlService(core: core, session: makeSession(authenticated: true))
        let attempts: [() async throws -> ItemLifecycleControlReceipt] = [
            { try await service.complete(commandID: "", itemID: "item-1", expectedRevision: 0) },
            { try await service.cancel(commandID: "c", itemID: "", expectedRevision: 0) },
            { try await service.notNow(commandID: "c", itemID: "item-1", expectedRevision: -1) },
        ]
        for attempt in attempts {
            do {
                _ = try await attempt()
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ItemLifecycleControlError, .invalidRequest)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testTheCoresRefusalsBecomeTypedErrors() async {
        let core = ScriptedItemLifecycleControlCore([
            .success(ItemLifecycleControlAnswer(outcome: "stale_revision", currentRevision: 7)),
            .success(ItemLifecycleControlAnswer(outcome: "command_conflict")),
            .success(ItemLifecycleControlAnswer(outcome: "item_deleted")),
            .success(ItemLifecycleControlAnswer(outcome: "item_ended", lifecycle: "completed")),
            .failure(CoreFailure(errorClass: .permanent, code: "not_found", message: "no stored record")),
            .failure(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")),
            .success(ItemLifecycleControlAnswer(outcome: "something_new")),
            .success(ItemLifecycleControlAnswer(outcome: "applied")),
        ])
        let service = ItemLifecycleControlService(core: core, session: makeSession(authenticated: true))
        var seen: [ItemLifecycleControlError] = []
        for index in 0..<8 {
            do {
                _ = try await service.complete(commandID: "c-\(index)", itemID: "item-1", expectedRevision: 0)
                XCTFail("expected a refusal")
            } catch {
                seen.append(error as! ItemLifecycleControlError)
            }
        }
        XCTAssertEqual(
            seen,
            [
                .staleRevision(currentRevision: 7), .commandConflict, .itemDeleted, .itemEnded(lifecycle: "completed"),
                .itemNotFound, .core(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")),
                .unreadable, .unreadable,
            ])
    }

    // MARK: Change signal

    func testEachControlThatTheCoreAppliedPublishesTheItemAndRefusalsPublishNothing() async {
        let publisher = RecordingChangePublisher()
        let core = ScriptedItemLifecycleControlCore([
            .success(applied()), .success(applied()), .success(applied()), .success(applied(replayed: true)),
            .success(ItemLifecycleControlAnswer(outcome: "stale_revision", currentRevision: 7)),
            .success(ItemLifecycleControlAnswer(outcome: "item_deleted")),
            .failure(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")),
        ])
        let service = ItemLifecycleControlService(
            core: core, session: makeSession(authenticated: true), changes: publisher)

        _ = try? await service.complete(commandID: "c-1", itemID: "item-1", expectedRevision: 2)
        _ = try? await service.cancel(commandID: "c-2", itemID: "item-1", expectedRevision: 3)
        _ = try? await service.notNow(commandID: "c-3", itemID: "item-1", expectedRevision: 4)
        _ = try? await service.stopSuggesting(commandID: "c-4", itemID: "item-1", expectedRevision: 5)
        XCTAssertEqual(publisher.changes, Array(repeating: .item("item-1"), count: 4))

        _ = try? await service.complete(commandID: "c-5", itemID: "item-1", expectedRevision: 2)
        _ = try? await service.cancel(commandID: "c-6", itemID: "item-1", expectedRevision: 2)
        _ = try? await service.notNow(commandID: "c-7", itemID: "item-1", expectedRevision: 2)
        XCTAssertEqual(publisher.changes.count, 4, "stale, deleted and failed commands announce nothing")
    }

    func testACaptureOnlySessionPublishesNothing() async {
        let publisher = RecordingChangePublisher()
        let service = ItemLifecycleControlService(
            core: ScriptedItemLifecycleControlCore([.success(applied())]), session: makeSession(authenticated: false),
            changes: publisher)
        _ = try? await service.complete(commandID: "c-1", itemID: "item-1", expectedRevision: 0)
        XCTAssertTrue(publisher.changes.isEmpty)
    }
}
