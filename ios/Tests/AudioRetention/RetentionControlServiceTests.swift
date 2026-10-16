import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A core that answers the retention reads and commands from a script and records what it was asked.
private final class ScriptedRetentionControlCore: RetentionControlCore, @unchecked Sendable {
    private let lock = NSLock()
    private var statusResult: Result<RetentionStatusAnswer, CoreFailure> = .success(
        RetentionStatusAnswer(policy: RetentionPolicyAnswer(periodSeconds: 604_800, isDefault: true), unresolved: []))
    private var audioResult: Result<RetentionAudioAnswer, CoreFailure> = .success(
        RetentionAudioAnswer(itemID: "item-1", audioReference: "FinalizedAudio/a.m4a", soleSource: false))
    private var controlResult: Result<RetentionControlAnswer, CoreFailure> = .success(
        RetentionControlAnswer(outcome: "stored", periodSeconds: 86_400))
    private(set) var statusReads = 0
    private(set) var audioReads: [String] = []
    private(set) var commands: [RetentionControlCommand] = []

    func script(status: Result<RetentionStatusAnswer, CoreFailure>) { locked { statusResult = status } }
    func script(audio: Result<RetentionAudioAnswer, CoreFailure>) { locked { audioResult = audio } }
    func script(control: Result<RetentionControlAnswer, CoreFailure>) { locked { controlResult = control } }

    func readStatus(completion: @escaping @Sendable (Result<RetentionStatusAnswer, CoreFailure>) -> Void) {
        completion(locked { statusReads += 1; return statusResult })
    }

    func readAudio(
        itemID: String, completion: @escaping @Sendable (Result<RetentionAudioAnswer, CoreFailure>) -> Void
    ) {
        completion(locked { audioReads.append(itemID); return audioResult })
    }

    func submit(
        _ command: RetentionControlCommand,
        completion: @escaping @Sendable (Result<RetentionControlAnswer, CoreFailure>) -> Void
    ) {
        completion(locked { commands.append(command); return controlResult })
    }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
}

private final class ScriptedUserRemover: UserAudioRemoving, @unchecked Sendable {
    private let lock = NSLock()
    private var outcome: AudioRemovalOutcome = .removed
    private var requests: [String] = []

    var requestedItems: [String] {
        lock.lock()
        defer { lock.unlock() }
        return requests
    }

    func script(_ outcome: AudioRemovalOutcome) {
        lock.lock()
        self.outcome = outcome
        lock.unlock()
    }

    func removeAudioForUser(itemID: String, audioReference: String) async -> AudioRemovalOutcome {
        lock.lock()
        defer { lock.unlock() }
        requests.append("\(itemID) \(audioReference)")
        return outcome
    }
}

/// `RetentionControlService` against a scripted core: the session check before every write, the refusals it maps, and
/// that a refused call reaches neither the core nor the remover.
final class RetentionControlServiceTests: XCTestCase {
    private let core = ScriptedRetentionControlCore()
    private let remover = ScriptedUserRemover()
    private var session: ReadAuthenticationSession!
    private var removerAvailable = true

    override func setUp() {
        super.setUp()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    private func makeService(pressure: CoreRetentionPressure = .normal) -> RetentionControlService {
        RetentionControlService(
            core: core, session: session, pressureProbe: FixedPressureProbe(pressure: pressure),
            removerProvider: { [unowned self] in removerAvailable ? remover : nil })
    }

    private func assertThrows(
        _ expected: RetentionControlError, file: StaticString = #filePath, line: UInt = #line,
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch {
            XCTAssertEqual(error as? RetentionControlError, expected, file: file, line: line)
        }
    }

    // MARK: Status

    func testStatusCarriesThePolicyThePressureAndTheUnresolvedCases() async throws {
        core.script(status: .success(RetentionStatusAnswer(
            policy: RetentionPolicyAnswer(periodSeconds: 3_600, isDefault: false),
            unresolved: [RetentionUnresolvedAnswer(
                itemID: "item-1", capturedAt: "2026-10-01T08:00:00Z", reason: "transcription_failed",
                options: ["retry_transcription", "delete_audio"])])))

        let status = try await makeService(pressure: .low).status()

        XCTAssertEqual(status.policy, RetentionPolicySetting(periodSeconds: 3_600, isDefault: false))
        XCTAssertEqual(status.storagePressure, .low)
        XCTAssertEqual(
            status.unresolved,
            [UnresolvedSoleAudioCase(
                itemID: "item-1", capturedAt: "2026-10-01T08:00:00Z", reason: .transcriptionFailed,
                options: [.retryTranscription, .deleteAudio])])
    }

    func testAnUnknownReasonOrOptionIsUnreadableRatherThanGuessed() async {
        for entry in [
            RetentionUnresolvedAnswer(itemID: "i", capturedAt: "t", reason: "new_reason", options: []),
            RetentionUnresolvedAnswer(itemID: "i", capturedAt: "t", reason: "transcription_failed", options: ["new"]),
        ] {
            core.script(status: .success(RetentionStatusAnswer(
                policy: RetentionPolicyAnswer(periodSeconds: 1, isDefault: true), unresolved: [entry])))
            let service = makeService()
            await assertThrows(.unreadable) { _ = try await service.status() }
        }
    }

    func testTheGateRefusalsAreNotAuthenticatedAndOtherFailuresAreTheCores() async {
        let service = makeService()
        for code in ["read_not_authenticated", "read_relocked"] {
            core.script(status: .failure(CoreFailure(errorClass: .permanent, code: code, message: "")))
            await assertThrows(.notAuthenticated) { _ = try await service.status() }
        }
        let other = CoreFailure(errorClass: .transient, code: "database_busy", message: "busy")
        core.script(status: .failure(other))
        await assertThrows(.core(other)) { _ = try await service.status() }
    }

    // MARK: Set period

    func testAPeriodInsideTheBoundsIsSentAndAnOutsideOneNeverLeavesTheService() async throws {
        let service = makeService()
        for outside in [Int64(0), -1, RetentionPolicySetting.maximumPeriodSeconds + 1] {
            await assertThrows(.invalidRequest) { _ = try await service.setPeriod(seconds: outside) }
        }
        XCTAssertEqual(core.commands, [])

        let stored = try await service.setPeriod(seconds: 86_400)
        XCTAssertEqual(stored, RetentionPolicySetting(periodSeconds: 86_400, isDefault: false))
        XCTAssertEqual(core.commands, [.setPeriod(seconds: 86_400)])
    }

    func testAWriteNeedsAnAuthenticatedSessionAndSendsNothingWithoutOne() async throws {
        let service = makeService()
        session.relock(.explicit)

        await assertThrows(.notAuthenticated) { _ = try await service.setPeriod(seconds: 86_400) }
        await assertThrows(.notAuthenticated) { _ = try await service.retryTranscription(itemID: "item-1") }
        await assertThrows(.notAuthenticated) { _ = try await service.deleteAudio(itemID: "item-1") }
        XCTAssertEqual(core.commands, [])
        XCTAssertEqual(core.audioReads, [])
        XCTAssertEqual(remover.requestedItems, [])
    }

    // MARK: Retry

    func testRetryReturnsTheQueuedJobAndMapsEachRefusal() async throws {
        let service = makeService()
        core.script(control: .success(RetentionControlAnswer(outcome: "requeued", jobID: "job-9")))
        let jobID = try await service.retryTranscription(itemID: "item-1")
        XCTAssertEqual(jobID, "job-9")
        XCTAssertEqual(core.commands, [.retryTranscription(itemID: "item-1")])

        let refusals: [(RetentionControlAnswer, RetentionControlError)] = [
            (RetentionControlAnswer(outcome: "item_deleted"), .itemDeleted),
            (RetentionControlAnswer(outcome: "audio_removed"), .audioRemoved),
            (RetentionControlAnswer(outcome: "job_in_flight"), .jobInFlight),
            (RetentionControlAnswer(outcome: "not_retryable", transcriptionState: "transcribed"),
             .notRetryable(transcriptionState: "transcribed")),
            (RetentionControlAnswer(outcome: "something_new"), .unreadable),
            (RetentionControlAnswer(outcome: "requeued"), .unreadable),
        ]
        for (answer, expected) in refusals {
            core.script(control: .success(answer))
            await assertThrows(expected) { _ = try await service.retryTranscription(itemID: "item-1") }
        }
        core.script(control: .failure(CoreFailure(errorClass: .permanent, code: "not_found", message: "")))
        await assertThrows(.itemNotFound) { _ = try await service.retryTranscription(itemID: "item-1") }
        await assertThrows(.invalidRequest) { _ = try await service.retryTranscription(itemID: "") }
    }

    // MARK: Delete audio

    func testDeleteAudioRemovesTheAudioTheCoreNamesAndReportsTheOutcome() async throws {
        let service = makeService()
        let receipt = try await service.deleteAudio(itemID: "item-1")
        XCTAssertEqual(receipt, .removed)
        XCTAssertEqual(remover.requestedItems, ["item-1 FinalizedAudio/a.m4a"])

        remover.script(.alreadyRemoved)
        let again = try await service.deleteAudio(itemID: "item-1")
        XCTAssertEqual(again, .alreadyRemoved)
    }

    func testDeleteAudioRefusesSoleSourceAudioBeforeAnythingIsRemoved() async {
        core.script(audio: .success(
            RetentionAudioAnswer(itemID: "item-1", audioReference: "FinalizedAudio/a.m4a", soleSource: true)))
        let service = makeService()

        await assertThrows(.soleSourceUseItemDeletion) { _ = try await service.deleteAudio(itemID: "item-1") }
        XCTAssertEqual(remover.requestedItems, [])
    }

    func testDeleteAudioForAnItemWithoutAudioOrWithoutARemoverChangesNothing() async {
        let service = makeService()
        core.script(audio: .success(RetentionAudioAnswer(itemID: "item-1", audioReference: nil, soleSource: false)))
        await assertThrows(.noAudio) { _ = try await service.deleteAudio(itemID: "item-1") }

        core.script(audio: .success(
            RetentionAudioAnswer(itemID: "item-1", audioReference: "FinalizedAudio/a.m4a", soleSource: false)))
        removerAvailable = false
        await assertThrows(.audioRemovalUnavailable) { _ = try await service.deleteAudio(itemID: "item-1") }
        await assertThrows(.invalidRequest) { _ = try await service.deleteAudio(itemID: "") }
        XCTAssertEqual(remover.requestedItems, [])
    }

    func testEachRemovalOutcomeThatIsNotSuccessIsReportedAsItsOwnError() async {
        let service = makeService()
        let outcomes: [(AudioRemovalOutcome, RetentionControlError)] = [
            (.denied(.soleSource), .soleSourceUseItemDeletion),
            (.denied(.deletionOwnsCleanup), .removalDenied(.deletionOwnsCleanup)),
            (.retryLater(.storageUnavailable), .audioRemovalIncomplete(.storageUnavailable)),
            (.retryLater(.ledgerUnavailable), .audioRemovalIncomplete(.ledgerUnavailable)),
            (.failedPermanently(.audioReferenceUnresolvable), .audioReferenceUnresolvable),
        ]
        for (outcome, expected) in outcomes {
            remover.script(outcome)
            await assertThrows(expected) { _ = try await service.deleteAudio(itemID: "item-1") }
        }
    }
}
