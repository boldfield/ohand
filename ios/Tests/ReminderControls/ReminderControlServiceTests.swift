import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each call with the next scripted result and records what the service asked.
private final class ScriptedReminderControlCore: ReminderControlCore, @unchecked Sendable {
    private let lock = NSLock()
    private var readScript: [Result<ReminderReadAnswer, CoreFailure>]
    private var historyScript: [Result<ReminderHistoryAnswer, CoreFailure>]
    private var commandScript: [Result<ReminderControlAnswer, CoreFailure>]
    private var receivedReads: [String] = []
    private var receivedHistories: [String] = []
    private var receivedCommands: [ReminderControlCommand] = []

    init(
        reads: [Result<ReminderReadAnswer, CoreFailure>] = [],
        histories: [Result<ReminderHistoryAnswer, CoreFailure>] = [],
        commands: [Result<ReminderControlAnswer, CoreFailure>] = []
    ) {
        readScript = reads
        historyScript = histories
        commandScript = commands
    }

    var reads: [String] { locked { receivedReads } }
    var histories: [String] { locked { receivedHistories } }
    var commands: [ReminderControlCommand] { locked { receivedCommands } }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }

    func readReminder(
        itemID: String, completion: @escaping @Sendable (Result<ReminderReadAnswer, CoreFailure>) -> Void
    ) {
        let next: Result<ReminderReadAnswer, CoreFailure>? = locked {
            receivedReads.append(itemID)
            return readScript.isEmpty ? nil : readScript.removeFirst()
        }
        completion(next ?? .failure(CoreReminderControlClient.unreadableAnswer))
    }

    func readHistory(
        itemID: String, completion: @escaping @Sendable (Result<ReminderHistoryAnswer, CoreFailure>) -> Void
    ) {
        let next: Result<ReminderHistoryAnswer, CoreFailure>? = locked {
            receivedHistories.append(itemID)
            return historyScript.isEmpty ? nil : historyScript.removeFirst()
        }
        completion(next ?? .failure(CoreReminderControlClient.unreadableAnswer))
    }

    func submit(
        _ command: ReminderControlCommand,
        completion: @escaping @Sendable (Result<ReminderControlAnswer, CoreFailure>) -> Void
    ) {
        let next: Result<ReminderControlAnswer, CoreFailure>? = locked {
            receivedCommands.append(command)
            return commandScript.isEmpty ? nil : commandScript.removeFirst()
        }
        completion(next ?? .failure(CoreReminderControlClient.unreadableAnswer))
    }
}

final class ReminderControlServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()
    private let instant = Date(timeIntervalSince1970: 4_000_000_000)

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func record(version: Int = 2, instant: String = "2096-10-02T09:46:40Z") -> ReminderRecordAnswer {
        ReminderRecordAnswer(
            reminderID: "rem-1", sourcePhrase: "tomorrow at nine", requestState: "resolved",
            scheduleState: "scheduled", resolvedInstant: instant, timezoneID: "Europe/Dublin", scheduleGeneration: 2,
            stateVersion: version, updatedAt: "2026-10-15T09:00:00Z")
    }

    private func applied(_ outcome: String = "corrected", replayed: Bool = false) -> ReminderControlAnswer {
        ReminderControlAnswer(outcome: outcome, itemID: "item-1", reminder: record(), replayed: replayed)
    }

    func testAReadReportsTheReminderWithItsVersionsAndInstant() async throws {
        let answer = ReminderReadAnswer(
            itemID: "item-1", itemRevision: 4, itemType: "action", lifecycleState: "active", reminder: record())
        let core = ScriptedReminderControlCore(reads: [.success(answer)])
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))

        let read = try await service.reminder(itemID: "item-1")

        XCTAssertEqual(read, answer)
        XCTAssertEqual(read.reminder?.resolvedDate, Date(timeIntervalSince1970: 4_000_000_000))
        XCTAssertEqual(core.reads, ["item-1"])
    }

    func testAReadRefusedByTheGateIsReportedAsNotAuthenticated() async {
        for code in ["read_not_authenticated", "read_relocked"] {
            let denial = CoreFailure(errorClass: .unauthorized, code: code, message: "refused")
            let core = ScriptedReminderControlCore(reads: [.failure(denial)], histories: [.failure(denial)])
            let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
            do {
                _ = try await service.reminder(itemID: "item-1")
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
            }
            do {
                _ = try await service.history(itemID: "item-1")
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
            }
        }
    }

    func testAnUnknownItemIsNotFoundOnEveryRead() async {
        let missing = CoreFailure(errorClass: .permanent, code: "not_found", message: "not found")
        let core = ScriptedReminderControlCore(reads: [.failure(missing)], histories: [.failure(missing)])
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
        do {
            _ = try await service.reminder(itemID: "item-1")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ReminderControlError, .itemNotFound)
        }
        do {
            _ = try await service.history(itemID: "item-1")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ReminderControlError, .itemNotFound)
        }
    }

    func testTheHistoryIsReturnedAsTheCoreBoundedIt() async throws {
        let history = ReminderHistoryAnswer(
            itemID: "item-1", reminderID: "rem-1", summary: nil, entries: [], totalEntries: 80, truncated: true)
        let core = ScriptedReminderControlCore(histories: [.success(history)])
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
        let read = try await service.history(itemID: "item-1")
        XCTAssertEqual(read, history)
        XCTAssertEqual(core.histories, ["item-1"])
    }

    func testAnEditSendsTheWholeSecondInstantAndTheVersionsTheUserSaw() async throws {
        let core = ScriptedReminderControlCore(commands: [.success(applied())])
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))

        let receipt = try await service.correctTime(
            commandID: "c-1", itemID: "item-1", expectedRevision: 4, expectedReminderVersion: 1,
            instant: instant.addingTimeInterval(0.75), timeZoneID: "Europe/Dublin")

        XCTAssertEqual(receipt, ReminderControlReceipt(itemID: "item-1", reminder: record(), replayed: false))
        XCTAssertEqual(
            core.commands,
            [
                ReminderControlCommand(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 4, expectedReminderVersion: 1,
                    change: .correctTime(instant: instant.addingTimeInterval(0.75), timeZoneID: "Europe/Dublin"))
            ])
        XCTAssertEqual(ReminderInstant.string(from: instant.addingTimeInterval(0.75)), "2096-10-02T09:46:40Z")
    }

    func testACancelSendsItsCommand() async throws {
        let core = ScriptedReminderControlCore(commands: [.success(applied("cancelled"))])
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
        _ = try await service.cancel(commandID: "c-1", itemID: "item-1", expectedRevision: 4, expectedReminderVersion: 2)
        XCTAssertEqual(
            core.commands,
            [
                ReminderControlCommand(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 4, expectedReminderVersion: 2,
                    change: .cancel)
            ])
    }

    func testEachRefusalIsReportedAsItsOwnError() async {
        let outcomes: [(ReminderControlAnswer, ReminderControlError)] = [
            (ReminderControlAnswer(outcome: "stale_revision", currentRevision: 7), .staleRevision(currentRevision: 7)),
            (
                ReminderControlAnswer(outcome: "stale_reminder_version", currentReminderVersion: 9),
                .staleReminderVersion(currentVersion: 9)
            ),
            (ReminderControlAnswer(outcome: "time_in_past"), .timeInPast),
            (ReminderControlAnswer(outcome: "command_conflict"), .commandConflict),
            (ReminderControlAnswer(outcome: "item_deleted"), .itemDeleted),
            (
                ReminderControlAnswer(outcome: "item_not_active", lifecycleState: "completed"),
                .itemNotActive(lifecycleState: "completed")
            ),
            (ReminderControlAnswer(outcome: "item_cannot_carry_reminder"), .itemCannotCarryReminder),
            (ReminderControlAnswer(outcome: "no_reminder"), .noReminder),
            (ReminderControlAnswer(outcome: "something_new"), .unreadable),
            (ReminderControlAnswer(outcome: "stale_revision"), .unreadable),
        ]
        for (answer, expected) in outcomes {
            let core = ScriptedReminderControlCore(commands: [.success(answer)])
            let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
            do {
                _ = try await service.correctTime(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0,
                    instant: instant, timeZoneID: "UTC")
                XCTFail("expected \(expected)")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, expected)
            }
        }
    }

    func testCoreFailuresAreMappedWithoutEchoingTheRequest() async {
        let missing = CoreFailure(errorClass: .permanent, code: "not_found", message: "not found")
        let broken = CoreFailure(errorClass: .transient, code: "storage_error", message: "storage failed")
        for (failure, expected) in [
            (missing, ReminderControlError.itemNotFound), (broken, ReminderControlError.core(broken)),
        ] {
            let core = ScriptedReminderControlCore(commands: [.failure(failure)])
            let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
            do {
                _ = try await service.cancel(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0)
                XCTFail("expected \(expected)")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, expected)
            }
        }
    }

    func testCaptureOnlyAndRelockedSessionsCannotWriteAndTheCoreIsNotAsked() async {
        let core = ScriptedReminderControlCore(commands: [.success(applied()), .success(applied())])
        let captureOnly = ReminderControlService(core: core, session: makeSession(authenticated: false))
        let relocking = makeSession(authenticated: true)
        let relocked = ReminderControlService(core: core, session: relocking)
        relocking.relock(.explicit)
        for service in [captureOnly, relocked] {
            do {
                _ = try await service.correctTime(
                    commandID: "c-1", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0,
                    instant: instant, timeZoneID: "UTC")
                XCTFail("a session without read scope must not edit")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
            }
            do {
                _ = try await service.cancel(
                    commandID: "c-2", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0)
                XCTFail("a session without read scope must not cancel")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let core = ScriptedReminderControlCore()
        let service = ReminderControlService(core: core, session: makeSession(authenticated: true))
        let attempts: [() async throws -> ReminderControlReceipt] = [
            {
                try await service.correctTime(
                    commandID: "", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0,
                    instant: self.instant, timeZoneID: "UTC")
            },
            {
                try await service.correctTime(
                    commandID: "c", itemID: "", expectedRevision: 0, expectedReminderVersion: 0,
                    instant: self.instant, timeZoneID: "UTC")
            },
            {
                try await service.correctTime(
                    commandID: "c", itemID: "item-1", expectedRevision: -1, expectedReminderVersion: 0,
                    instant: self.instant, timeZoneID: "UTC")
            },
            {
                try await service.correctTime(
                    commandID: "c", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: -1,
                    instant: self.instant, timeZoneID: "UTC")
            },
            {
                try await service.correctTime(
                    commandID: "c", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: 0,
                    instant: self.instant, timeZoneID: "Mars/Olympus_Mons")
            },
            { try await service.cancel(commandID: "c", itemID: "item-1", expectedRevision: 0, expectedReminderVersion: -1) },
        ]
        for attempt in attempts {
            do {
                _ = try await attempt()
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? ReminderControlError, .invalidRequest)
            }
        }
        do {
            _ = try await service.reminder(itemID: "")
            XCTFail("expected a refusal")
        } catch {
            XCTAssertEqual(error as? ReminderControlError, .invalidRequest)
        }
        XCTAssertTrue(core.commands.isEmpty)
        XCTAssertTrue(core.reads.isEmpty)
    }
}
