import Foundation
import UIKit

/// Registers the launch and foreground hooks that read the notification permission again.
/// Becoming active covers both a return from Settings and any foreground event; each runs the
/// same idempotent, single-flight refresh, so repeated activations cannot repeat an effect.
/// Failures are reported in the report, not thrown; cancellation produces no report.
public final class NotificationPermissionLifecycleObserver {
    private let reconciler: NotificationPermissionReconciler
    private let notificationCenter: NotificationCenter
    private let onReport: @Sendable (NotificationPermissionReport) -> Void
    private var token: NSObjectProtocol?

    public init(
        reconciler: NotificationPermissionReconciler,
        notificationCenter: NotificationCenter = .default,
        onReport: @escaping @Sendable (NotificationPermissionReport) -> Void
    ) {
        self.reconciler = reconciler
        self.notificationCenter = notificationCenter
        self.onReport = onReport
    }

    /// Registers the hook and, when `isActive`, refreshes at once as the launch pass.
    public func start(isActive: Bool) {
        guard token == nil else { return }
        token = notificationCenter.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.refresh(.foreground)
        }
        if isActive { refresh(.launch) }
    }

    public func stop() {
        if let token { notificationCenter.removeObserver(token) }
        token = nil
    }

    private func refresh(_ trigger: NotificationReconcileTrigger) {
        let reconciler = self.reconciler
        let onReport = self.onReport
        Task {
            if let report = try? await reconciler.refresh(trigger: trigger) {
                onReport(report)
            }
        }
    }
}
