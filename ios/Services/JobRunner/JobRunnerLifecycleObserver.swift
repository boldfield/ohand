import Foundation
import UIKit

/// A grant of extra execution time while the app is in the background.
public protocol BackgroundExecutionGrant: AnyObject {
    /// Asks for time. `expiration` runs if the system takes it back. Returns a token for `end`.
    func begin(expiration: @escaping @Sendable () -> Void) -> Int
    func end(_ token: Int)
}

/// `UIApplication`-backed grant. Use it from the main thread.
public final class ApplicationBackgroundExecutionGrant: BackgroundExecutionGrant {
    public init() {}

    public func begin(expiration: @escaping @Sendable () -> Void) -> Int {
        UIApplication.shared.beginBackgroundTask(withName: "com.boldfield.ohand.job-drain") {
            expiration()
        }.rawValue
    }

    public func end(_ token: Int) {
        UIApplication.shared.endBackgroundTask(UIBackgroundTaskIdentifier(rawValue: token))
    }
}

/// Registers the launch, foreground, reachability and suspension hooks that drive a `JobRunnerService`.
///
/// Becoming active and the network returning start a drain; entering the background asks the running drain to
/// stop at a checkpoint and holds a background-execution grant open until the core confirms it stopped (or the
/// system takes the grant back); termination takes the same checkpoint. Every notification is idempotent, so
/// repeated activations cannot start duplicate drainers. All hooks are removed by `stop()`.
public final class JobRunnerLifecycleObserver {
    private let service: JobRunnerService
    private let notificationCenter: NotificationCenter
    private let reachability: NetworkReachability?
    private let backgroundGrant: BackgroundExecutionGrant?
    private var tokens: [NSObjectProtocol] = []
    private var isStarted = false

    public init(
        service: JobRunnerService,
        notificationCenter: NotificationCenter = .default,
        reachability: NetworkReachability? = nil,
        backgroundGrant: BackgroundExecutionGrant? = nil
    ) {
        self.service = service
        self.notificationCenter = notificationCenter
        self.reachability = reachability
        self.backgroundGrant = backgroundGrant
    }

    /// Registers the hooks and, when `isActive`, runs the launch drain at once. Notifications are expected on the
    /// main queue. Reachability comes only from the monitor's callbacks, the first of which reports the current state;
    /// reading its snapshot here could race the monitor and land a stale value after a newer one.
    public func start(isActive: Bool) {
        guard !isStarted else { return }
        isStarted = true

        tokens.append(
            notificationCenter.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) {
                [service] _ in service.activate()
            })
        tokens.append(
            notificationCenter.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) {
                [weak self] _ in self?.enterBackground()
            })
        tokens.append(
            notificationCenter.addObserver(forName: UIApplication.willTerminateNotification, object: nil, queue: .main) {
                [service] _ in service.terminate()
            })
        reachability?.startMonitoring { [service] reachable in
            service.reachabilityChanged(reachable)
        }
        if isActive {
            service.activate()
        }
    }

    public func stop() {
        guard isStarted else { return }
        isStarted = false
        tokens.forEach { notificationCenter.removeObserver($0) }
        tokens = []
        reachability?.stopMonitoring()
    }

    private func enterBackground() {
        service.suspend()
        guard let backgroundGrant else { return }
        let box = TokenBox()
        let token = backgroundGrant.begin { [weak backgroundGrant] in
            if let held = box.take() {
                backgroundGrant?.end(held)
            }
        }
        if !box.set(token) {
            backgroundGrant.end(token)
            return
        }
        service.whenIdle { [weak backgroundGrant] in
            if let held = box.take() {
                backgroundGrant?.end(held)
            }
        }
    }
}

/// Hands a background-task token to whichever of the expiration handler and the idle callback runs first.
private final class TokenBox: @unchecked Sendable {
    private let lock = NSLock()
    private var token: Int?
    private var wasTaken = false

    /// Returns `false` when the token was already claimed, so the caller must end it itself.
    func set(_ value: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !wasTaken else { return false }
        token = value
        return true
    }

    func take() -> Int? {
        lock.lock()
        defer { lock.unlock() }
        wasTaken = true
        let value = token
        token = nil
        return value
    }
}
