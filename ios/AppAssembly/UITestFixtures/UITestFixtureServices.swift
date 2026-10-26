#if DEBUG
import AVFoundation
import Foundation
import OhAndCoreBridge
import OhAndServices
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

    init(name: String) {
        self.name = name
    }

    func makeServices() throws -> AppServices {
        guard let fixture = UITestFixture(rawValue: name) else { throw UITestFixtureError.unknownFixture(name) }
        let storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-ui-fixture-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)

        let sender = UITestFixtureSender()
        let recognizer = UITestFixtureRecognizer()
        let permission: NotificationAuthorization = fixture == .emptyFirstRun ? .notDetermined : .authorized
        let lifecycle = LifecycleCoordinator()
        let retrieval = RetrievalSearchModule()
        let root = try ServiceCompositionRoot(
            uiTestFixture: AppConfiguration.uiTestFixture(storageRoot: storageRoot),
            sender: sender,
            notificationCenter: UITestFixtureNotificationCenter(permission: permission),
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

    /// Fills the store once the services have started. It runs on the main actor after launch returns, so the first
    /// screen can appear before the fixture is complete; a test waits for the content it checks.
    func seedInBackground() {
        guard let pendingSeed else { return }
        let fixtureName = name
        Task { @MainActor in
            let seeder = UITestFixtureSeeder(
                fixture: pendingSeed.fixture, services: pendingSeed.services, sender: pendingSeed.sender,
                recognizer: pendingSeed.recognizer)
            do {
                try await seeder.seed()
            } catch {
                NSLog("UITestFixture \(fixtureName) seeding failed: \(error)")
            }
        }
    }
}

/// Fills the store of one fixture through the root's own operations.
@MainActor
final class UITestFixtureSeeder {
    private let fixture: UITestFixture
    private let services: AppServices
    private let sender: UITestFixtureSender
    private let recognizer: UITestFixtureRecognizer
    private var root: ServiceCompositionRoot { services.root }

    init(
        fixture: UITestFixture, services: AppServices, sender: UITestFixtureSender, recognizer: UITestFixtureRecognizer
    ) {
        self.fixture = fixture
        self.services = services
        self.sender = sender
        self.recognizer = recognizer
    }

    func seed() async throws {
        // Provider setup is a private operation, so the locked fixture is unlocked while it is filled and locked again
        // before the test sees it.
        if fixture == .locked {
            let outcome = await withCheckedContinuation { continuation in
                root.authenticateReads(reason: "Prepare the locked fixture") { continuation.resume(returning: $0) }
            }
            guard outcome == .authenticated else { throw UITestFixtureError.seedingFailed("unlock: \(outcome)") }
        }
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
                "Remind me soon to call the plumber about the leaking tap",
                reply: .proposal(
                    try Self.proposal(
                        text: "Remind me soon to call the plumber about the leaking tap", itemType: "action",
                        reminder: .ambiguous(phrase: "soon"))))
        case .failedSorting:
            try await configureProvider()
            try await saveTyped("Compare the synthetic rejected quotes for the shed roof", reply: .rejected)
        case .offline:
            try await configureProvider()
            try await saveTyped("Synthetic waiting note about the garden gate", reply: .unreachable)
            try await saveTyped("Synthetic waiting idea about a compost bin", reply: .unreachable)
        }
        services.homeRecents.loadFirstPage()
    }

    // MARK: Fixture content

    private func seedPopulated() async throws {
        let typedNote = "Synthetic typed note about the garden hose"
        let typedIdea = "Synthetic typed idea about a herb planter"
        let typedAction = "Synthetic typed action to sweep the porch"
        let reminderText = "Remind me \(UITestFixtureLaunch.sortedReminderPhrase) to call the roofer about the estimate"
        try await saveTyped(typedNote, reply: .proposal(try Self.proposal(text: typedNote, itemType: "note")))
        try await saveTyped(typedIdea, reply: .proposal(try Self.proposal(text: typedIdea, itemType: "idea")))
        try await saveTyped(typedAction, reply: .proposal(try Self.proposal(text: typedAction, itemType: "action")))
        try await saveTyped(
            reminderText,
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
                transcript: transcript, reply: .proposal(try Self.proposal(text: transcript, itemType: itemType)))
        }
    }

    // MARK: Production operations

    /// Stores a synthetic key, creates a text-interpretation profile, makes it the default and grants it to the capture
    /// route, the way provider settings do.
    private func configureProvider() async throws {
        let reference = try root.providerCredentials.addCredential(Data("synthetic-fixture-key".utf8))
        let configuration = root.providerConfiguration
        let created = try await configuration.createProfile(
            ProviderProfileDraft(
                profileID: "profile-ui-fixture", providerProtocol: "anthropic", model: "synthetic-model",
                credentialReference: reference, timeoutSeconds: 30))
        try await configuration.setDefault(capability: "text_interpretation", profileVersion: created.profileVersion)
        _ = try await configuration.grant(
            routeID: AppConfiguration.universalRouteID, capability: "text_interpretation",
            profileVersion: created.profileVersion)
    }

    private func saveTyped(_ text: String, reply: UITestFixtureProviderReply) async throws {
        sender.script(phrase: text, reply: reply)
        let outcome = await withCheckedContinuation { continuation in
            root.saveText(captureID: "fixture-\(UUID().uuidString.lowercased())", text: text, capturedAt: Date()) {
                continuation.resume(returning: $0)
            }
        }
        guard case .saved = outcome else { throw UITestFixtureError.seedingFailed("typed capture not saved: \(outcome)") }
    }

    /// Records a short synthetic clip and hands it off as a finished recording; the scripted recognizer attaches
    /// `transcript`, which the scripted provider then interprets.
    private func saveVoice(transcript: String, reply: UITestFixtureProviderReply) async throws {
        let captureID = "fixture-\(UUID().uuidString.lowercased())"
        let fileName = "\(captureID).wav"
        try Self.writeSyntheticClip(to: root.inProgressAudioDirectory.appendingPathComponent(fileName))
        recognizer.script(captureID: captureID, transcript: transcript)
        sender.script(phrase: transcript, reply: reply)
        let handoff = CompositionRecordingHandoff(
            captureID: captureID, inProgressFileName: fileName, finalizedFileName: fileName, startedAt: Date())
        let outcome = await withCheckedContinuation { continuation in
            root.saveRecording(handoff) { continuation.resume(returning: $0) }
        }
        guard case .saved = outcome else { throw UITestFixtureError.seedingFailed("recording not saved: \(outcome)") }
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
