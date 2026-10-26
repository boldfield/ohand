#if DEBUG
import Foundation

/// The named states a UI test can open the app in. Debug builds only: this file and the rest of `UITestFixtures/` are
/// compiled out of a release build, so a release app has no path to a fixture.
enum UITestFixture: String, CaseIterable {
    /// Notification permission not yet asked, nothing saved.
    case emptyFirstRun = "empty-first-run"
    /// A populated store whose reads are locked until the person unlocks.
    case locked
    /// Typed and voice notes of each item type, and one sorted reminder.
    case populated
    /// A reminder whose time the provider could not resolve.
    case needsCorrection = "needs-correction"
    /// One note whose provider request was rejected.
    case failedSorting = "failed-sorting"
    /// The network is unavailable and saved notes wait for it.
    case offline

    /// The launch argument that selects a fixture: `-OhAndUITestFixture <name>`.
    static let launchArgument = "-OhAndUITestFixture"

    /// The name following the launch argument, or nil when the app was launched without one.
    static func requestedName(arguments: [String] = ProcessInfo.processInfo.arguments) -> String? {
        guard let flagIndex = arguments.firstIndex(of: launchArgument), arguments.indices.contains(flagIndex + 1) else {
            return nil
        }
        return arguments[flagIndex + 1]
    }
}
#endif
