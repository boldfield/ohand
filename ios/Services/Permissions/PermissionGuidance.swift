import Foundation

/// What a person can do about a permission that is not granted.
public enum PermissionRecovery: Equatable, Sendable {
    /// Nothing to do: the app asks when the feature is first used, or the permission is granted.
    case none
    /// The permission can be switched on in iOS Settings.
    case openSettings
    /// A device policy applies; Settings cannot change it.
    case managedByDevice
}

/// The plain words for one permission, in one state. Each says what still works, because capture never depends on any of
/// them.
public struct PermissionGuidance: Equatable, Sendable {
    public let headline: String
    public let detail: String
    public let recovery: PermissionRecovery

    public static func make(kind: PermissionKind, status: PermissionStatus, change: PermissionChange = .unchanged) -> PermissionGuidance {
        let recovery: PermissionRecovery
        switch status {
        case .granted, .notRequested: recovery = .none
        case .denied: recovery = .openSettings
        case .restricted: recovery = .managedByDevice
        }
        return PermissionGuidance(
            headline: headline(kind: kind, status: status, change: change),
            detail: detail(kind: kind, status: status),
            recovery: recovery)
    }

    private static func name(_ kind: PermissionKind) -> String {
        switch kind {
        case .microphone: return "Microphone"
        case .speech: return "Speech recognition"
        case .notifications: return "Notifications"
        }
    }

    private static func headline(kind: PermissionKind, status: PermissionStatus, change: PermissionChange) -> String {
        let name = name(kind)
        switch (status, change) {
        case (.granted, .restored): return "\(name) is on again"
        case (.granted, _): return "\(name) is on"
        case (.notRequested, _): return "\(name) has not been asked for yet"
        case (.denied, .revoked): return "\(name) was turned off"
        case (.denied, _): return "\(name) is off"
        case (.restricted, _): return "\(name) is restricted on this device"
        }
    }

    private static func detail(kind: PermissionKind, status: PermissionStatus) -> String {
        if status == .restricted {
            return "A device setting (such as Screen Time or a management profile) prevents this, and Oh And cannot change it. Capture still works: everything you type or record is saved and can be found again."
        }
        switch (kind, status) {
        case (.microphone, .granted):
            return "You can record a voice capture."
        case (.microphone, .notRequested):
            return "Oh And asks the first time you start a recording. Typed and silent captures never need it."
        case (.microphone, _):
            return "Voice recording is unavailable. Typed and silent captures still save and can be found again. To record, allow Microphone for Oh And in iOS Settings."
        case (.speech, .granted):
            return "Recordings are transcribed on this device. Audio is not sent anywhere to be transcribed."
        case (.speech, .notRequested):
            return "Oh And asks the first time a recording needs a transcript. Recordings are saved either way."
        case (.speech, _):
            return "Recordings are still saved as audio and nothing is sent anywhere to be transcribed. Transcripts wait until you allow Speech Recognition for Oh And in iOS Settings."
        case (.notifications, .granted):
            return "Reminders and the daily prompt can alert you."
        case (.notifications, .notRequested):
            return "Oh And asks the first time you set a reminder or turn on the daily prompt."
        case (.notifications, _):
            return "Reminders are saved but cannot alert you, and the daily prompt stays quiet. Everything you capture is saved and searchable. To be alerted, allow Notifications for Oh And in iOS Settings."
        }
    }
}
