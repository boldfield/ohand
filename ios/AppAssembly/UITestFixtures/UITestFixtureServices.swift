#if DEBUG
import AVFoundation
import Foundation
import OhAndCoreBridge
import OhAndServices
import SQLite3
import UserNotifications

/// Why a fixture launch could not be built.
enum UITestFixtureError: Error {
    case unknownFixture(String)
    case seedingFailed(String)
}

/// Builds the app over an isolated store for one named fixture. Nothing here opens the production store or the
/// Keychain items production uses: the store is a fresh temporary directory, the credentials use a service name made for
/// this launch, and the provider, recognizer, notification center and read session are scripted substitutes. The store
/// is filled through the same operations the app runs (save, provider setup, grant, interpretation), never with SQL.
final class UITestFixtureLaunch {
    /// Placed far in the future so a reminder stays in the future on any real date.
    static let sortedReminderPhrase = "2099-01-15 11:00:00"

    private let name: String
    private var pendingSeed: (fixture: UITestFixture, services: AppServices, sender: UITestFixtureSender, recognizer: UITestFixtureRecognizer)?
    private var storageRoot: URL?
    /// The fake notification center of this launch, for tests of the fixture itself.
    private(set) var notificationCenter: UITestFixtureNotificationCenter?

    init(name: String) {
        self.name = name
    }

    func makeServices() throws -> AppServices {
        guard let fixture = UITestFixture(rawValue: name) else { throw UITestFixtureError.unknownFixture(name) }
        let storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-ui-fixture-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        self.storageRoot = storageRoot

        let sender = UITestFixtureSender()
        let recognizer = UITestFixtureRecognizer()
        let permission: NotificationAuthorization = fixture == .emptyFirstRun ? .notDetermined : .authorized
        let notificationCenter = UITestFixtureNotificationCenter(permission: permission)
        self.notificationCenter = notificationCenter
        let lifecycle = LifecycleCoordinator()
        let retrieval = RetrievalSearchModule()
        let root = try ServiceCompositionRoot(
            uiTestFixture: AppConfiguration.uiTestFixture(storageRoot: storageRoot),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            sender: sender,
            notificationCenter: notificationCenter,
            reachability: UITestFixtureReachability(isReachable: fixture != .offline),
            recognizer: recognizer,
            readsStartUnlocked: fixture != .locked,
            modules: [lifecycle, retrieval])
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: .live, retrieval: retrieval,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationPermission: UITestFixturePermissionAsker(
                state: permission == .authorized ? .usable : .notRequested))
        pendingSeed = (fixture, services, sender, recognizer)
        return services
    }

    /// One value read straight from the store file, for the message of a failing fixture test.
    func diagnosticScalar(_ sql: String) -> String {
        guard let storageRoot,
            let file = FileManager.default.enumerator(at: storageRoot, includingPropertiesForKeys: nil)?
                .compactMap({ $0 as? URL }).first(where: { $0.lastPathComponent == "core.sqlite" })
        else { return "no store file" }
        var database: OpaquePointer?
        guard sqlite3_open_v2(file.path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { return "unreadable" }
        defer { sqlite3_close(database) }
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return "bad query" }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "no row" }
        return String(cString: text)
    }

    /// Fills the store once the services have started. It runs on the main actor after launch returns, so the first
    /// screen can appear before the fixture is complete. The status marker (`ui-fixture.status`) reads `seeding` until
    /// the store is filled and every note has settled, then `ready`; a seeding failure reads `failed: <reason>`.
    func seedInBackground() {
        Task { @MainActor in
            let marker = UITestFixtureStatusMarker()
            marker.show("seeding")
            do {
                _ = try await seedAndWait()
                marker.show("ready")
            } catch {
                NSLog("UITestFixture \(name) seeding failed: \(error)")
                marker.show("failed: \(error)")
            }
        }
    }

    /// Fills the store and returns once every seeded note has reached the state the fixture names.
    @MainActor
    func seedAndWait() async throws -> UITestFixtureSeeder {
        guard let pendingSeed else { throw UITestFixtureError.seedingFailed("services were not made") }
        let seeder = UITestFixtureSeeder(
            fixture: pendingSeed.fixture, services: pendingSeed.services, sender: pendingSeed.sender,
            recognizer: pendingSeed.recognizer, storageRoot: storageRoot)
        try await seeder.seed()
        return seeder
    }
}

/// The state a seeded note is expected to reach before the fixture is ready.
enum UITestFixtureExpectation {
    /// Interpreted by the scripted provider.
    case processed
    /// Interpreted, with a reminder request whose time is not resolved (needs correction).
    case needsCorrection
    /// The provider rejected the request: a permanent failure, the note stays uninterpreted.
    case rejected
    /// Waiting for a later attempt: not interpreted and not failed.
    case waiting

    func isReached(by report: ItemStatusReport) -> Bool {
        switch self {
        case .processed:
            return report.processingState == "processed" && Self.hasTranscript(report)
        case .needsCorrection:
            return report.processingState == "processed" && report.reminderRequestState == "not_scheduled_yet"
        case .rejected:
            return report.processingState == "uninterpreted"
        case .waiting:
            return report.processingState == "unprocessed"
        }
    }

    private static func hasTranscript(_ report: ItemStatusReport) -> Bool {
        report.transcriptionState == "not_applicable" || report.transcriptionState == "transcribed"
    }
}

/// One note a fixture saved, with the status the core reported once it settled.
struct UITestFixtureSeededNote {
    let text: String
    let itemID: String
    let expectation: UITestFixtureExpectation
    var settledStatus: ItemStatusReport?
}

/// Fills the store of one fixture through the root's own operations.
@MainActor
final class UITestFixtureSeeder {
    private let fixture: UITestFixture
    private let services: AppServices
    private let sender: UITestFixtureSender
    private let recognizer: UITestFixtureRecognizer
    private let storageRoot: URL?
    private var root: ServiceCompositionRoot { services.root }
    /// The notes saved so far, in order, with the status each had when the fixture was ready.
    private(set) var notes: [UITestFixtureSeededNote] = []

    init(
        fixture: UITestFixture, services: AppServices, sender: UITestFixtureSender, recognizer: UITestFixtureRecognizer,
        storageRoot: URL? = nil
    ) {
        self.storageRoot = storageRoot
        self.fixture = fixture
        self.services = services
        self.sender = sender
        self.recognizer = recognizer
    }

    func seed() async throws {
        // Provider setup and the status reads are private operations, so every fixture is unlocked while it is filled,
        // and the locked one is locked again before the test sees it.
        try await unlockForSeeding()
        defer { if fixture == .locked { root.relockReads() } }
        switch fixture {
        case .emptyFirstRun:
            return
        case .populated, .locked:
            try await configureProvider()
            try await seedPopulated()
        case .needsCorrection:
            try await configureProvider()
            try await saveTyped(
                "Remind me soon to call the plumber about the leaking tap", expecting: .needsCorrection,
                reply: .proposal(
                    try Self.proposal(
                        text: "Remind me soon to call the plumber about the leaking tap", itemType: "action",
                        reminder: .ambiguous(phrase: "soon"))))
        case .failedSorting:
            try await configureProvider()
            try await saveTyped("Compare the synthetic rejected quotes for the shed roof", expecting: .rejected, reply: .rejected)
        case .offline:
            try await configureProvider()
            try await saveTyped("Synthetic waiting note about the garden gate", expecting: .waiting, reply: .unreachable)
            try await saveTyped("Synthetic waiting idea about a compost bin", expecting: .waiting, reply: .unreachable)
        }
        try await settleNotes()
        services.homeRecents.loadFirstPage()
    }

    /// Authenticates the read session unless it already is. The scene's own foreground event can relock the session
    /// while the fixture is still filling, so this runs before every private step that can wait.
    private func unlockForSeeding() async throws {
        guard root.readScope != .authenticated else { return }
        let outcome = await withCheckedContinuation { continuation in
            root.authenticateReads(reason: "Prepare the fixture") { continuation.resume(returning: $0) }
        }
        guard outcome == .authenticated else { throw UITestFixtureError.seedingFailed("unlock: \(outcome)") }
    }

    // MARK: Settling

    /// Waits until every saved note shows the state its fixture names, reading the status the way the app does. The
    /// locked fixture is still unlocked here, so the reads are allowed.
    private func settleNotes() async throws {
        let deadline = Date().addingTimeInterval(120)
        for index in notes.indices {
            let note = notes[index]
            var lastReport: ItemStatusReport?
            while true {
                try await unlockForSeeding()
                let result = await withCheckedContinuation { continuation in
                    root.itemStatus(itemID: note.itemID) { continuation.resume(returning: $0) }
                }
                if case .success(let report) = result {
                    lastReport = report
                    if note.expectation.isReached(by: report) {
                        notes[index].settledStatus = report
                        break
                    }
                }
                guard Date() < deadline else {
                    throw UITestFixtureError.seedingFailed(
                        "'\(note.text)' did not settle; processing \(lastReport?.processingState ?? "unread"), job \(lastReport?.processingJobStatus ?? "none"), reminder \(lastReport?.reminderRequestState ?? "none"); provider: \(sender.attemptSummary); jobs: \(jobRows(of: note.itemID))")
                }
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }
    }

    /// The job rows of one item, read straight from the store file, for the message of a fixture that did not settle.
    private func jobRows(of itemID: String) -> String {
        guard let storageRoot,
            let file = FileManager.default.enumerator(at: storageRoot, includingPropertiesForKeys: nil)?
                .compactMap({ $0 as? URL }).first(where: { $0.lastPathComponent == "core.sqlite" })
        else { return "no store file" }
        var database: OpaquePointer?
        guard sqlite3_open_v2(file.path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { return "unreadable" }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 2000)
        var statement: OpaquePointer?
        let query = "SELECT status, COALESCE(failure_reason, '-') FROM jobs WHERE item_id = ?1"
        guard sqlite3_prepare_v2(database, query, -1, &statement, nil) == SQLITE_OK else { return "no jobs table" }
        defer { sqlite3_finalize(statement) }
        sqlite3_bind_text(statement, 1, itemID, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
        var rows: [String] = []
        while sqlite3_step(statement) == SQLITE_ROW {
            let status = sqlite3_column_text(statement, 0).map { String(cString: $0) } ?? "?"
            let reason = sqlite3_column_text(statement, 1).map { String(cString: $0) } ?? "?"
            rows.append("\(status)/\(reason)")
        }
        return rows.isEmpty ? "none" : rows.joined(separator: ",")
    }

    // MARK: Fixture content

    private func seedPopulated() async throws {
        let typedNote = "Synthetic typed note about the garden hose"
        let typedIdea = "Synthetic typed idea about a herb planter"
        let typedAction = "Synthetic typed action to sweep the porch"
        let reminderText = "Remind me \(UITestFixtureLaunch.sortedReminderPhrase) to call the roofer about the estimate"
        try await saveTyped(typedNote, expecting: .processed, reply: .proposal(try Self.proposal(text: typedNote, itemType: "note")))
        try await saveTyped(typedIdea, expecting: .processed, reply: .proposal(try Self.proposal(text: typedIdea, itemType: "idea")))
        try await saveTyped(typedAction, expecting: .processed, reply: .proposal(try Self.proposal(text: typedAction, itemType: "action")))
        try await saveTyped(
            reminderText, expecting: .processed,
            reply: .proposal(
                try Self.proposal(
                    text: reminderText, itemType: "action",
                    reminder: .explicit(phrase: UITestFixtureLaunch.sortedReminderPhrase, instant: "2099-01-15T11:00:00Z"))))
        let spokenTexts = [
            ("note", "Synthetic spoken note about the bookshelf"),
            ("idea", "Synthetic spoken idea about a window box"),
            ("action", "Synthetic spoken action to water the ferns"),
        ]
        for (itemType, transcript) in spokenTexts {
            try await saveVoice(
                transcript: transcript, expecting: .processed, reply: .proposal(try Self.proposal(text: transcript, itemType: itemType)))
        }
    }

    // MARK: Production operations

    /// Stores a synthetic key, creates a text-interpretation profile, makes it the default and grants it to the capture
    /// route, the way provider settings do.
    private func configureProvider() async throws {
        try await unlockForSeeding()
        let reference = try root.providerCredentials.addCredential(Data("synthetic-fixture-key".utf8))
        let configuration = root.providerConfiguration
        let created = try await configuration.createProfile(
            ProviderProfileDraft(
                profileID: "profile-ui-fixture", providerProtocol: "anthropic", model: "synthetic-model",
                credentialReference: reference, timeoutSeconds: 30))
        try await configuration.setDefault(capability: "text_interpretation", profileVersion: created.profileVersion)
        let disposition = try await configuration.grant(
            routeID: AppConfiguration.universalRouteID, capability: "text_interpretation",
            profileVersion: created.profileVersion)
        guard disposition == .granted || disposition == .unchanged else { throw UITestFixtureError.seedingFailed("grant: \(disposition)") }
    }

    private func saveTyped(_ text: String, expecting expectation: UITestFixtureExpectation, reply: UITestFixtureProviderReply) async throws {
        sender.script(phrase: text, reply: reply)
        let outcome = await withCheckedContinuation { continuation in
            root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) {
                continuation.resume(returning: $0)
            }
        }
        guard case .saved(_, let itemID, _) = outcome else {
            throw UITestFixtureError.seedingFailed("typed capture not saved: \(outcome)")
        }
        notes.append(UITestFixtureSeededNote(text: text, itemID: itemID, expectation: expectation))
    }

    /// Records a short synthetic clip and hands it off as a finished recording; the scripted recognizer attaches
    /// `transcript`, which the scripted provider then interprets.
    private func saveVoice(transcript: String, expecting expectation: UITestFixtureExpectation, reply: UITestFixtureProviderReply) async throws {
        let captureID = UUID().uuidString.lowercased()
        let fileName = "\(captureID).wav"
        try Self.writeSyntheticClip(to: root.inProgressAudioDirectory.appendingPathComponent(fileName))
        recognizer.script(captureID: captureID, transcript: transcript)
        sender.script(phrase: transcript, reply: reply)
        let handoff = CompositionRecordingHandoff(
            captureID: captureID, inProgressFileName: fileName, finalizedFileName: fileName, startedAt: Date())
        let outcome = await withCheckedContinuation { continuation in
            root.saveRecording(handoff) { continuation.resume(returning: $0) }
        }
        guard case .saved(_, let itemID, _, _) = outcome else {
            throw UITestFixtureError.seedingFailed("recording not saved: \(outcome)")
        }
        notes.append(UITestFixtureSeededNote(text: transcript, itemID: itemID, expectation: expectation))
    }

    // MARK: Synthetic content

    enum ReminderScript {
        case explicit(phrase: String, instant: String)
        case ambiguous(phrase: String)
    }

    /// An Anthropic message that answers the interpretation tool with one proposal over the whole text.
    static func proposal(text: String, itemType: String, reminder: ReminderScript? = nil) throws -> Data {
        var proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": itemType,
            "source_spans": [["start": 0, "end": text.count]],
        ]
        if let reminder {
            let phrase: String
            var reminderProposal: [String: Any] = [:]
            switch reminder {
            case .explicit(let explicitPhrase, let instant):
                phrase = explicitPhrase
                reminderProposal["instant"] = instant
                reminderProposal["timezone_id"] = "UTC"
                reminderProposal["quality"] = "explicit"
            case .ambiguous(let ambiguousPhrase):
                phrase = ambiguousPhrase
                reminderProposal["quality"] = "ambiguous"
            }
            guard let range = text.range(of: phrase) else { throw UITestFixtureError.seedingFailed("no phrase") }
            let start = text.distance(from: text.startIndex, to: range.lowerBound)
            reminderProposal["source_span"] = ["start": start, "end": start + phrase.count]
            proposal["reminder_proposal"] = reminderProposal
        }
        let message: [String: Any] = [
            "id": "msg_ui_fixture_\(UUID().uuidString.prefix(8))",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_ui_fixture", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: message)
    }

    /// A half-second of low-level 16 kHz mono PCM, enough for the audio store to accept as a recording.
    static func writeSyntheticClip(to destination: URL) throws {
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM), AVSampleRateKey: 16000.0, AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16, AVLinearPCMIsBigEndianKey: false, AVLinearPCMIsFloatKey: false,
        ]
        let file = try AVAudioFile(
            forWriting: destination, settings: settings, commonFormat: .pcmFormatInt16, interleaved: true)
        let frames: AVAudioFrameCount = 8000
        guard let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: frames),
            let samples = buffer.int16ChannelData?[0]
        else { throw UITestFixtureError.seedingFailed("no audio buffer") }
        buffer.frameLength = frames
        for index in 0..<Int(frames) { samples[index] = Int16(truncatingIfNeeded: index % 100) }
        try file.write(from: buffer)
    }
}
#endif
