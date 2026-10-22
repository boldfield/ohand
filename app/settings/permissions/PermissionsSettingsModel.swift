import Combine
import Foundation
import OhAndServices

/// The permissions this screen can read. `PermissionsOverview` is the production implementation.
protocol PermissionsReading: Sendable {
    func refresh() async -> [PermissionEntry]
}

extension PermissionsOverview: PermissionsReading {}

/// One permission as the screen shows it.
struct PermissionRow: Equatable, Identifiable {
    let kind: PermissionKind
    let status: PermissionStatus
    let change: PermissionChange
    let guidance: PermissionGuidance

    var id: PermissionKind { kind }
}

/// The state of the permissions screen.
///
/// Rules this type keeps:
/// * It only reads. Opening it, coming back from iOS Settings and every refresh ask for nothing.
/// * A permission that is off never disables anything here or elsewhere: each row says what still works.
/// * A permission that was granted and is now off is called out as turned off, so the person is not left guessing.
/// Confined to the main queue.
final class PermissionsSettingsModel: ObservableObject {
    @Published private(set) var rows: [PermissionRow] = []
    @Published private(set) var hasLoaded = false

    private let overview: PermissionsReading
    private let openSystemSettings: () -> Void

    init(overview: PermissionsReading, openSystemSettings: @escaping () -> Void) {
        self.overview = overview
        self.openSystemSettings = openSystemSettings
    }

    /// Reads every permission again. Call it when the screen appears and whenever the app becomes active, which is how a
    /// return from iOS Settings arrives.
    @MainActor func refresh() async {
        let entries = await overview.refresh()
        rows = entries.map { entry in
            PermissionRow(
                kind: entry.kind, status: entry.status, change: entry.change,
                guidance: .make(kind: entry.kind, status: entry.status, change: entry.change))
        }
        hasLoaded = true
    }

    /// Opens the app's page in iOS Settings, where the permission can be switched on again.
    func openSettings() {
        openSystemSettings()
    }
}
