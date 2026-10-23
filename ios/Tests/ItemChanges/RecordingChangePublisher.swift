import Foundation
@testable import OhAndServices

/// Records every change a service publishes, so a test can say what was announced and what was not.
final class RecordingChangePublisher: ItemChangePublishing, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [ItemChange] = []

    func publish(_ change: ItemChange) {
        lock.lock()
        stored.append(change)
        lock.unlock()
    }

    var changes: [ItemChange] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }
}
