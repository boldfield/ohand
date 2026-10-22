import AVFoundation
import Foundation
import Speech

/// `PermissionStatusReading` over the real frameworks. It only reads; asking is done at each feature's own boundary.
public struct SystemPermissionStatusReader: PermissionStatusReading {
    private let notifications: NotificationCenterProviding

    public init(notifications: NotificationCenterProviding) {
        self.notifications = notifications
    }

    public func status(of kind: PermissionKind) async -> PermissionStatus {
        switch kind {
        case .microphone: return Self.microphoneStatus()
        case .speech: return Self.speechStatus(SFSpeechRecognizer.authorizationStatus())
        case .notifications: return Self.notificationStatus(await notifications.authorization())
        }
    }

    static func microphoneStatus() -> PermissionStatus {
        if #available(iOS 17.0, *) {
            switch AVAudioApplication.shared.recordPermission {
            case .granted: return .granted
            case .denied: return .denied
            default: return .notRequested
            }
        } else {
            switch AVAudioSession.sharedInstance().recordPermission {
            case .granted: return .granted
            case .denied: return .denied
            default: return .notRequested
            }
        }
    }

    static func speechStatus(_ status: SFSpeechRecognizerAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized: return .granted
        case .denied: return .denied
        case .restricted: return .restricted
        case .notDetermined: return .notRequested
        @unknown default: return .denied
        }
    }

    static func notificationStatus(_ authorization: NotificationAuthorization) -> PermissionStatus {
        switch NotificationPermissionState(authorization) {
        case .usable: return .granted
        case .notRequested: return .notRequested
        case .denied: return .denied
        }
    }
}
