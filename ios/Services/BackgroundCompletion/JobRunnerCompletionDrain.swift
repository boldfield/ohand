import Foundation

/// `BackgroundCompletionDrain` over a `JobRunnerService` that leaves the runner's single drain-idle slot to its
/// owner.
///
/// The runner holds one drain-idle observer and the composition root already uses it for the reminder pass. This
/// adapter keeps the completion service's observer in its own slot; the owner calls `drainWentIdle()` from the
/// runner's observer, so the reminder pass and the completion service both run after every drain and neither
/// replaces the other.
public final class JobRunnerCompletionDrain: BackgroundCompletionDrain, @unchecked Sendable {
    private let runner: JobRunnerService
    private let lock = NSLock()
    private var observer: (@Sendable () -> Void)?

    public init(runner: JobRunnerService) {
        self.runner = runner
    }

    public var isDraining: Bool { runner.isDraining }

    public func beginCompletionWindow() { runner.beginCompletionWindow() }

    public func endCompletionWindow() { runner.endCompletionWindow() }

    public func requestDrain() { runner.requestDrain() }

    public func setDrainIdleObserver(_ observer: (@Sendable () -> Void)?) {
        lock.lock()
        self.observer = observer
        lock.unlock()
    }

    /// Forwards one drain-idle event to the completion service's observer, if one is set.
    public func drainWentIdle() {
        lock.lock()
        let current = observer
        lock.unlock()
        current?()
    }
}
