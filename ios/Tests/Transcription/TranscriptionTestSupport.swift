import CryptoKit
import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A recognizer that answers from a script and records every call. It never touches the network.
final class FakeOnDeviceRecognizer: OnDeviceRecognizing, @unchecked Sendable {
    let recognizerName = "fake-on-device/1.0"
    private let lock = NSLock()
    private var outcome: OnDeviceRecognitionOutcome
    private var holdsUntilCancelled: Bool
    private var calls: [(url: URL, locale: String)] = []
    private var sawCancellation = false
    private var onRecognize: (@Sendable (URL) -> Void)?

    init(outcome: OnDeviceRecognitionOutcome, holdsUntilCancelled: Bool = false) {
        self.outcome = outcome
        self.holdsUntilCancelled = holdsUntilCancelled
    }

    var callCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return calls.count
    }

    var locales: [String] {
        lock.lock()
        defer { lock.unlock() }
        return calls.map(\.locale)
    }

    var urls: [URL] {
        lock.lock()
        defer { lock.unlock() }
        return calls.map(\.url)
    }

    var wasCancelledWhileRecognizing: Bool {
        lock.lock()
        defer { lock.unlock() }
        return sawCancellation
    }

    func setOutcome(_ value: OnDeviceRecognitionOutcome, holdsUntilCancelled hold: Bool = false) {
        lock.lock()
        outcome = value
        holdsUntilCancelled = hold
        lock.unlock()
    }

    func setOnRecognize(_ action: (@Sendable (URL) -> Void)?) {
        lock.lock()
        onRecognize = action
        lock.unlock()
    }

    func recognize(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome {
        lock.lock()
        calls.append((audioURL, localeIdentifier))
        let hold = holdsUntilCancelled
        let answer = outcome
        let action = onRecognize
        lock.unlock()
        action?(audioURL)
        while hold {
            if Task.isCancelled {
                lock.lock()
                sawCancellation = true
                lock.unlock()
                return .cancelled
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return answer
    }
}

/// Core operations answered from a script, for handler tests that need no core.
final class FakeTranscriptionCore: TranscriptionCoreOperations, @unchecked Sendable {
    private let lock = NSLock()
    private var source: Result<TranscriptionJobSource, CoreFailure>
    private var applyResults: [Result<TranscriptionApplyAcknowledgment, CoreFailure>]
    private var appliedOutcomes: [TranscriptionOutcomeRequest] = []
    private var reads: [(jobID: String, attempt: Int)] = []

    init(
        source: Result<TranscriptionJobSource, CoreFailure>,
        applyResults: [Result<TranscriptionApplyAcknowledgment, CoreFailure>] = []
    ) {
        self.source = source
        self.applyResults = applyResults
    }

    var applied: [TranscriptionOutcomeRequest] {
        lock.lock()
        defer { lock.unlock() }
        return appliedOutcomes
    }

    var readCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return reads.count
    }

    func readSource(jobID: String, attempt: Int) async -> Result<TranscriptionJobSource, CoreFailure> {
        lock.lock()
        defer { lock.unlock() }
        reads.append((jobID, attempt))
        return source
    }

    /// Answers each call with the next scripted result; the last one repeats.
    func apply(_ outcome: TranscriptionOutcomeRequest) async -> Result<TranscriptionApplyAcknowledgment, CoreFailure> {
        lock.lock()
        defer { lock.unlock() }
        appliedOutcomes.append(outcome)
        if applyResults.count > 1 { return applyResults.removeFirst() }
        return applyResults.first ?? .success(TranscriptionFixtures.acknowledgment(.attached))
    }
}

/// Maps references to files in a temporary directory, the way the protected-storage locator does for the device.
struct DirectoryAudioLocator: TranscriptionAudioLocating {
    let directory: URL

    func audioURL(forReference reference: String) -> URL? {
        let prefix = "FinalizedAudio/"
        guard reference.hasPrefix(prefix) else { return nil }
        return directory.appendingPathComponent(String(reference.dropFirst(prefix.count)))
    }
}

enum TranscriptionFixtures {
    static let jobID = "job-transcribe-1"
    static let itemID = "5a1c0000-0000-4000-8000-0000000000e1"
    static let captureID = "5a1c0000-0000-4000-8000-0000000000c1"
    static let audioBytes = Data("synthetic audio bytes, not a recording".utf8)

    static func sha256Hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    static func source(
        audioReference: String? = "FinalizedAudio/clip-1.m4a",
        locale: String = "en_US",
        lifecycleState: String = "active",
        transcriptionState: String = "audio_pending",
        leaseAttempt: Int = 1
    ) -> TranscriptionJobSource {
        var fields: [String: Any] = [
            "operation_id": 1, "job_id": jobID, "item_id": itemID, "capture_id": captureID,
            "lease_attempt": leaseAttempt, "locale": locale, "lifecycle_state": lifecycleState,
            "transcription_state": transcriptionState,
        ]
        if let audioReference {
            fields["audio_reference"] = audioReference
        } else {
            fields["audio_reference"] = NSNull()
        }
        return decode(fields)
    }

    static func acknowledgment(_ disposition: TranscriptionApplyAcknowledgment.Disposition) -> TranscriptionApplyAcknowledgment {
        decode([
            "operation_id": 2, "item_id": itemID, "job_id": jobID, "disposition": disposition.rawValue,
            "revision": 1, "transcription_state": "transcribed", "discard_reason": NSNull(),
            "interpretation": ["status": "not_requested", "job_id": NSNull(), "denial": NSNull()],
        ])
    }

    private static func decode<Value: Decodable>(_ fields: [String: Any]) -> Value {
        do {
            let data = try JSONSerialization.data(withJSONObject: fields)
            return try JSONDecoder().decode(Value.self, from: data)
        } catch {
            fatalError("fixture could not be decoded: \(error)")
        }
    }

    static func failure(_ code: String, _ errorClass: CoreErrorClass = .permanent) -> CoreFailure {
        CoreFailure(errorClass: errorClass, code: code, message: "fixture")
    }
}

/// Synthetic stored state for one voice capture and its queued transcription job, written the way a foreground
/// import and the job queue would, plus straight reads of the rows the tests check.
struct TranscriptionStore {
    let path: String

    struct Item: Equatable {
        let lifecycleState: String
        let transcriptionState: String
        let revision: Int
    }

    struct Attachment: Equatable {
        let text: String
        let audioSHA256: String
        let recognizer: String
        let sourceJobID: String
    }

    /// Creates the store with the real core so its migrations run, closes it, then seeds the rows.
    static func make(at path: String) throws -> TranscriptionStore {
        try CoreHandle(path: path).close()
        return TranscriptionStore(path: path)
    }

    func seedVoiceCapture(
        audioReference: String,
        locale: String = "en_US",
        lifecycleState: String = "active",
        transcriptionState: String = "audio_pending",
        jobStatus: String? = "queued",
        attemptCount: Int = 0,
        leaseExpiresAt: String? = nil
    ) throws {
        var statements = [
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, \
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) VALUES \
            ('\(TranscriptionFixtures.captureID)', NULL, '\(audioReference)', '2026-10-08T09:30:00Z', 'UTC', 0, \
            '\(locale)', 'gregorian', 'personal', 'route-personal', 0, '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, \
            processing_state, transcription_state, created_at, updated_at) VALUES \
            ('\(TranscriptionFixtures.itemID)', '\(TranscriptionFixtures.captureID)', 0, '\(lifecycleState)', \
            'saved_local', 'not_configured', 'unprocessed', '\(transcriptionState)', '2026-10-08T09:30:01Z', \
            '2026-10-08T09:30:01Z')
            """,
        ]
        if let jobStatus {
            let lease = leaseExpiresAt.map { "'\($0)'" } ?? "NULL"
            statements.append(
                """
                INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
                request_version, status, attempt_count, lease_expires_at, created_at) VALUES \
                ('\(TranscriptionFixtures.jobID)', 1, '\(TranscriptionFixtures.itemID)', 'transcription_attachment', \
                0, NULL, NULL, '\(jobStatus)', \(attemptCount), \(lease), '2026-10-08T09:30:02Z')
                """)
        }
        try execute(statements)
    }

    func execute(_ statements: [String]) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 2000)
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw SeedFailure.statementFailed }
        }
    }

    enum SeedFailure: Error {
        case statementFailed
    }

    var job: StoredJob? { JobStoreInspector.job(at: path, id: TranscriptionFixtures.jobID) }

    var item: Item? {
        query("SELECT lifecycle_state, transcription_state, revision FROM items WHERE item_id = ?",
              TranscriptionFixtures.itemID) { statement in
            Item(
                lifecycleState: Self.text(statement, 0), transcriptionState: Self.text(statement, 1),
                revision: Int(sqlite3_column_int(statement, 2)))
        }
    }

    var attachments: [Attachment] {
        queryAll("SELECT text, audio_sha256, recognizer, source_job_id FROM transcript_attachments WHERE item_id = ?",
                 TranscriptionFixtures.itemID) { statement in
            Attachment(
                text: Self.text(statement, 0), audioSHA256: Self.text(statement, 1),
                recognizer: Self.text(statement, 2), sourceJobID: Self.text(statement, 3))
        }
    }

    var captureText: String? {
        query("SELECT text FROM captures WHERE capture_id = ?", TranscriptionFixtures.captureID) { statement in
            sqlite3_column_text(statement, 0).map { String(cString: $0) }
        } ?? nil
    }

    func count(of table: String) -> Int {
        query("SELECT COUNT(*) FROM \(table)", nil) { Int(sqlite3_column_int($0, 0)) } ?? -1
    }

    private static func text(_ statement: OpaquePointer?, _ column: Int32) -> String {
        sqlite3_column_text(statement, column).map { String(cString: $0) } ?? ""
    }

    private func query<Value>(_ sql: String, _ argument: String?, _ read: (OpaquePointer?) -> Value) -> Value? {
        queryAll(sql, argument, read).first
    }

    private func queryAll<Value>(_ sql: String, _ argument: String?, _ read: (OpaquePointer?) -> Value) -> [Value] {
        var database: OpaquePointer?
        guard sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 2000)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_finalize(statement) }
        if let argument {
            sqlite3_bind_text(statement, 1, argument, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
        }
        var rows: [Value] = []
        while sqlite3_step(statement) == SQLITE_ROW {
            rows.append(read(statement))
        }
        return rows
    }
}
