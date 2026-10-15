import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each command with the next scripted result and records what the service asked.
private final class ScriptedItemCorrectionCore: ItemCorrectionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ItemCorrectionAnswer, CoreFailure>]
    private var received: [ItemCorrectionCommand] = []

    init(_ script: [Result<ItemCorrectionAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [ItemCorrectionCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: ItemCorrectionCommand,
        completion: @escaping @Sendable (Result<ItemCorrectionAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreItemCorrectionClient.unreadableAnswer))
    }
}

final class ItemCorrectionServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func corrected(revision: Int = 3, replayed: Bool = false, cancelled: Bool = false) -> ItemCorrectionAnswer {
        ItemCorrectionAnswer(
            outcome: "corrected", itemID: "item-1", revision: revision, replayed: replayed,
            reminderCancelled: cancelled)
    }

    func testEachCorrectionKindSendsItsCommandAndReturnsTheReceipt() async throws {
        let core = ScriptedItemCorrectionCore([
            .success(corrected(revision: 3)), .success(corrected(revision: 4, cancelled: true)),
            .success(corrected(revision: 5)),
        ])
        let service = ItemCorrectionService(core: core, session: makeSession(authenticated: true))

        let text = try await service.correctText(
            commandID: "c-1", itemID: "item-1", expectedRevision: 2, text: "call the roofer")
        let type = try await service.correctType(
            commandID: "c-2", itemID: "item-1", expectedRevision: 3, itemType: .note)
        let topic = try await service.correctSessionTopic(
            commandID: "c-3", itemID: "item-1", expectedRevision: 4, sessionTopic: "garage")

        XCTAssertEqual(text, ItemCorrectionReceipt(itemID: "item-1", revision: 3, replayed: false, reminderCancelled: false))
        XCTAssertTrue(type.reminderCancelled)
        XCTAssertEqual(topic.revision, 5)
        XCTAssertEqual(
            core.commands,
            [
                ItemCorrectionCommand(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 2, change: .text("call the roofer")),
                ItemCorrectionCommand(
                    commandID: "c-2", itemID: "item-1", expectedRevision: 3, change: .itemType(.note)),
                ItemCorrectionCommand(
                    commandID: "c-3", itemID: "item-1", expectedRevision: 4, change: .sessionTopic("garage")),
            ])
    }

    func testCaptureOnlySessionIsRefusedAndTheCoreIsNotAsked() async {
        let core = ScriptedItemCorrectionCore([.success(corrected())])
        let service = ItemCorrectionService(core: core, session: makeSession(authenticated: false))
        do {
            _ = try await service.correctText(commandID: "c-1", itemID: "item-1", expectedRevision: 0, text: "x")
            XCTFail("a capture-only session must not correct")
        } catch {
            XCTAssertEqual(error as? ItemCorrectionError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testRelockedSessionIsRefusedAgain() async {
        let core = ScriptedItemCorrectionCore([.success(corrected())])
        let session = makeSession(authenticated: true)
        let service = ItemCorrectionService(core: core, session: session)
        session.relock(.explicit)
        do {
            _ = try await service.correctType(commandID: "c-1", itemID: "item-1", expectedRevision: 0, itemType: .idea)
            XCTFail("a relocked session must not correct")
        } catch {
            XCTAssertEqual(error as? ItemCorrectionError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let core = ScriptedItemCorrectionCore([])
        let service = ItemCorrectionService(core: core, session: makeSession(authenticated: true))
        let attempts: [() async throws -> ItemCorrectionReceipt] = [
            { try await service.correctText(commandID: "", itemID: "item-1", expectedRevision: 0, text: "x") },
            { try await service.correctText(commandID: "c", itemID: "", expectedRevision: 0, text: "x") },
            { try await service.correctText(commandID: "c", itemID: "item-1", expectedRevision: -1, text: "x") },
            { try await service.correctText(commandID: "c", itemID: "item-1", expectedRevision: 0, text: "  \n") },
            { try await service.correctSessionTopic(commandID: "c", itemID: "item-1", expectedRevision: 0, sessionTopic: "") },
        ]
        for attempt in attempts {
            do {
                _ = try await attempt()
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ItemCorrectionError, .invalidRequest)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testTheCoresRefusalsBecomeTypedErrors() async {
        let core = ScriptedItemCorrectionCore([
            .success(ItemCorrectionAnswer(outcome: "stale_revision", currentRevision: 7)),
            .success(ItemCorrectionAnswer(outcome: "command_conflict")),
            .success(ItemCorrectionAnswer(outcome: "item_deleted")),
            .failure(CoreFailure(errorClass: .permanent, code: "not_found", message: "no stored record")),
            .failure(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")),
            .success(ItemCorrectionAnswer(outcome: "something_new")),
            .success(ItemCorrectionAnswer(outcome: "corrected")),
        ])
        let service = ItemCorrectionService(core: core, session: makeSession(authenticated: true))
        var seen: [ItemCorrectionError] = []
        for index in 0..<7 {
            do {
                _ = try await service.correctText(
                    commandID: "c-\(index)", itemID: "item-1", expectedRevision: 0, text: "x")
                XCTFail("expected a refusal")
            } catch {
                seen.append(error as! ItemCorrectionError)
            }
        }
        XCTAssertEqual(
            seen,
            [
                .staleRevision(currentRevision: 7), .commandConflict, .itemDeleted, .itemNotFound,
                .core(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")), .unreadable,
                .unreadable,
            ])
    }
}
