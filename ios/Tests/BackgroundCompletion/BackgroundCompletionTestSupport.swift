import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A grant the test drives: it records begin and end, and fires the expiration handler the way the system does.
final class FakeBackgroundGrant: BackgroundExecutionGrant, @unchecked Sendable {
    private let lock = NSLock()
    private var expiration: (@Sendable () -> Void)?
    private var begun = 0
    private var endedTokens: [Int] = []

    /// 0 is `UIBackgroundTaskIdentifier.invalid`.
    var tokenToReturn = 7
    /// Runs the expiration handler before `begin` returns, as the system does when it cannot grant time.
    var expiresDuringBegin = false

    var beginCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return begun
    }

    var endedTokenList: [Int] {
        lock.lock()
        defer { lock.unlock() }
        return endedTokens
    }

    func begin(expiration: @escaping @Sendable () -> Void) -> Int {
        lock.lock()
        begun += 1
        self.expiration = expiration
        let token = tokenToReturn
        let fireNow = expiresDuringBegin
        lock.unlock()
        if fireNow { expiration() }
        return token
    }

    func end(_ token: Int) {
        lock.lock()
        endedTokens.append(token)
        lock.unlock()
    }

    func fireExpiration() {
        lock.lock()
        let handler = expiration
        lock.unlock()
        handler?()
    }
}

final class FakeCompletionDrain: BackgroundCompletionDrain, @unchecked Sendable {
    private let lock = NSLock()
    private var draining = false
    private var windowBegins = 0
    private var windowEnds = 0
    private var requests = 0

    /// Whether closing the window stops the running drain, as the real checkpoint does.
    var stopsWhenWindowEnds = true
    /// Whether opening the window starts a drain.
    var startsDrainWhenWindowOpens = true

    var isDraining: Bool {
        lock.lock()
        defer { lock.unlock() }
        return draining
    }

    var windowBeginCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return windowBegins
    }

    var windowEndCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return windowEnds
    }

    var drainRequestCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return requests
    }

    func beginCompletionWindow() {
        lock.lock()
        windowBegins += 1
        if startsDrainWhenWindowOpens { draining = true }
        lock.unlock()
    }

    func endCompletionWindow() {
        lock.lock()
        windowEnds += 1
        if stopsWhenWindowEnds { draining = false }
        lock.unlock()
    }

    func requestDrain() {
        lock.lock()
        requests += 1
        lock.unlock()
    }

    func finishDrain() {
        lock.lock()
        draining = false
        lock.unlock()
    }

    private var idleObserver: (@Sendable () -> Void)?

    func setDrainIdleObserver(_ observer: (@Sendable () -> Void)?) {
        lock.lock()
        idleObserver = observer
        lock.unlock()
    }

    /// Reports a drain ending outside any lease, as the job runner does for foreground and reachability drains.
    func fireDrainIdle() {
        lock.lock()
        let observer = idleObserver
        lock.unlock()
        observer?()
    }
}

final class FakePendingReader: PendingProcessingReading, @unchecked Sendable {
    private let lock = NSLock()
    private var current: PendingProcessingStatus

    init(_ status: PendingProcessingStatus) {
        current = status
    }

    func set(_ status: PendingProcessingStatus) {
        lock.lock()
        current = status
        lock.unlock()
    }

    func read() async -> PendingProcessingStatus {
        lock.lock()
        defer { lock.unlock() }
        return current
    }
}

final class FakePendingAlert: PendingProcessingAlerting, @unchecked Sendable {
    private let lock = NSLock()
    private var arms = 0
    private var disarms = 0

    var armCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return arms
    }

    var disarmCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return disarms
    }

    func arm() async -> PendingProcessingAlertResult {
        lock.lock()
        arms += 1
        lock.unlock()
        return .armed(dueInstant: Date().addingTimeInterval(120))
    }

    func disarm() async {
        lock.lock()
        disarms += 1
        lock.unlock()
    }
}

/// Polls until `condition` holds, letting the main queue run (core events arrive there).
@MainActor
func waitUntilTrue(timeout: TimeInterval = 15, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
    return condition()
}

/// A real core over a seeded store with the job runner and a health reader attached, and a fake OS notification
/// center and background grant around it. Events arrive on the main queue.
final class BackgroundCompletionSession {
    let core: CoreHandle
    let service: JobRunnerService
    let health: ProcessingHealthService
    let storePath: String
    let center = FakeNotificationCenter()
    let grant = FakeBackgroundGrant()
    private var isClosed = false

    init(
        storePath: String,
        sender: ProviderRequestSender,
        capabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = true
    ) throws {
        self.storePath = storePath
        core = try CoreHandle(path: storePath)
        service = try JobRunnerService.attach(
            to: core, storePath: storePath, sender: sender, capabilities: capabilities,
            isReachable: isReachable, retryDelay: 3600)
        health = ProcessingHealthService(core: core)
        try core.setEventHandler { [unowned self] event in
            if !self.service.handle(event) {
                _ = self.health.handle(event)
            }
        }
    }

    @MainActor
    func makeCompletion(
        maximumWindow: TimeInterval = 20, settleTimeout: TimeInterval = 5, alertDelay: TimeInterval = 120
    ) throws -> BackgroundCompletionService {
        let policy = try XCTUnwrap(PendingProcessingAlertPolicy(delay: alertDelay))
        return BackgroundCompletionService(
            drain: service, grant: grant,
            pending: ProcessingHealthPendingReader(service: health),
            alert: PendingProcessingAlert(center: center, policy: policy),
            maximumWindow: maximumWindow, settleTimeout: settleTimeout, pollInterval: 0.01)
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        service.shutDown()
        core.close()
    }
}
