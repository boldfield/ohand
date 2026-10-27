#if DEBUG
import Foundation
import OhAndServices

/// What the scripted provider answers for a request that carries a given phrase.
enum UITestFixtureProviderReply {
    /// A finished interpretation (a provider message body).
    case proposal(Data)
    /// The provider refuses the request outright. It reaches the core as a permanent `rejected` failure, so the note ends
    /// uninterpreted rather than waiting for a credential or the network.
    case rejected
    /// The provider cannot be reached: the job waits for a later attempt.
    case unreachable
}

/// A provider that answers from fixtures and never touches the network. A request whose body carries none of the
/// scripted phrases is treated as unreachable.
final class UITestFixtureSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [(phrase: String, reply: UITestFixtureProviderReply)] = []

    func script(phrase: String, reply: UITestFixtureProviderReply) {
        lock.lock()
        replies.append((phrase, reply))
        lock.unlock()
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        let requestBody = String(decoding: request.body ?? Data(), as: UTF8.self)
        lock.lock()
        let matched = replies.first { requestBody.contains($0.phrase) }?.reply
        lock.unlock()
        switch matched ?? .unreachable {
        case .proposal(let body):
            return ProviderHTTPResponse(status: 200, headers: ["Content-Type": "application/json"], body: body)
        case .rejected:
            throw ProviderTransportError.invalidRequest(.invalidLimits)
        case .unreachable:
            throw ProviderTransportError.connectionFailed
        }
    }
}

/// A scripted on-device recognizer: the transcript of a recording is chosen by the capture ID its file name starts with.
final class UITestFixtureRecognizer: OnDeviceRecognizing, @unchecked Sendable {
    let recognizerName = "ui-test-fixture-recognizer/1.0"
    private let lock = NSLock()
    private var transcripts: [String: String] = [:]

    func script(captureID: String, transcript: String) {
        lock.lock()
        transcripts[captureID] = transcript
        lock.unlock()
    }

    func recognize(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome {
        let fileName = audioURL.lastPathComponent
        lock.lock()
        let transcript = transcripts.first { fileName.hasPrefix($0.key) }?.value
        lock.unlock()
        guard let transcript else { return .failed }
        return .transcript(text: transcript, confidence: 0.9)
    }
}

/// A notification center that holds requests in memory and reports the permission state the fixture chose.
final class UITestFixtureNotificationCenter: NotificationCenterProviding, @unchecked Sendable {
    private let lock = NSLock()
    private let permission: NotificationAuthorization
    private var pending: [String: NotificationCenterRequest] = [:]

    init(permission: NotificationAuthorization) {
        self.permission = permission
    }

    func authorization() async -> NotificationAuthorization {
        permission
    }

    /// The permission state the fixture chose, for tests of the fixture itself.
    var chosenPermission: NotificationAuthorization { permission }

    /// The due instants of the requests added so far.
    var pendingDueInstants: [Date] {
        lock.lock()
        defer { lock.unlock() }
        return pending.values.map(\.dueInstant)
    }

    func add(_ request: NotificationCenterRequest) async throws {
        guard permission == .authorized else { return }
        lock.lock()
        pending[request.identifier] = request
        lock.unlock()
    }

    func pendingRequests() async throws -> [NotificationCenterPendingRequest] {
        lock.lock()
        defer { lock.unlock() }
        return pending.values.map {
            NotificationCenterPendingRequest(
                identifier: $0.identifier, dueInstant: $0.dueInstant, userInfo: $0.content.userInfo)
        }
    }

    func removePending(identifiers: [String]) async {
        lock.lock()
        identifiers.forEach { pending[$0] = nil }
        lock.unlock()
    }

    func deliveredNotifications() async -> [NotificationCenterDeliveredNotification] {
        []
    }
}

/// Answers the notification permission question from the fixture, so the system prompt is never shown.
struct UITestFixturePermissionAsker: NotificationPermissionAsking {
    let state: NotificationPermissionState

    func askIfNeverRequested() async -> NotificationPermissionAskOutcome {
        .alreadyDecided(state)
    }
}

/// Network reachability the fixture fixes for the whole launch.
final class UITestFixtureReachability: NetworkReachability, @unchecked Sendable {
    let isReachable: Bool

    init(isReachable: Bool) {
        self.isReachable = isReachable
    }

    func startMonitoring(_ onChange: @escaping @Sendable (Bool) -> Void) {
        onChange(isReachable)
    }

    func stopMonitoring() {}
}
#endif
