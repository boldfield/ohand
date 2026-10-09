import Foundation
import Network

/// Whether the device can currently reach the network at all. The job loop uses it only to avoid starting provider
/// work that is certain to fail and burn a job's retry budget.
public protocol NetworkReachability: AnyObject {
    var isReachable: Bool { get }
    /// Starts reporting. `onChange` runs on an unspecified queue for every change, and once for the current state.
    func startMonitoring(_ onChange: @escaping @Sendable (Bool) -> Void)
    func stopMonitoring()
}

/// `NWPathMonitor`-backed reachability.
public final class SystemNetworkReachability: NetworkReachability, @unchecked Sendable {
    private let queue = DispatchQueue(label: "com.boldfield.ohand.reachability")
    private let stateLock = NSLock()
    private var monitor: NWPathMonitor?
    private var latest = false

    public init() {}

    public var isReachable: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return latest
    }

    public func startMonitoring(_ onChange: @escaping @Sendable (Bool) -> Void) {
        stateLock.lock()
        monitor?.cancel()
        let newMonitor = NWPathMonitor()
        monitor = newMonitor
        stateLock.unlock()

        newMonitor.pathUpdateHandler = { [weak self] path in
            let reachable = path.status == .satisfied
            self?.record(reachable)
            onChange(reachable)
        }
        newMonitor.start(queue: queue)
    }

    public func stopMonitoring() {
        stateLock.lock()
        let activeMonitor = monitor
        monitor = nil
        stateLock.unlock()
        activeMonitor?.cancel()
    }

    private func record(_ reachable: Bool) {
        stateLock.lock()
        latest = reachable
        stateLock.unlock()
    }
}
