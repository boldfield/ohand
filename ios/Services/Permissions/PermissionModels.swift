import Foundation

/// The three OS permissions the app asks for, each only at the feature that needs it: the microphone when a recording
/// starts, speech recognition when a recording is transcribed on the device, and notifications when a reminder or the
/// daily prompt first needs them. None is needed to save a typed or silent capture and read it back.
public enum PermissionKind: String, CaseIterable, Sendable {
    case microphone
    case speech
    case notifications
}

/// What the OS currently says, reduced to what the app can act on. `restricted` means a device policy (Screen Time or a
/// profile) prevents the person from changing it, so Settings cannot help.
public enum PermissionStatus: Equatable, Sendable {
    case notRequested
    case granted
    case denied
    case restricted

    public var isGranted: Bool { self == .granted }
}

/// How a permission moved since the previous time it was read.
public enum PermissionChange: Equatable, Sendable {
    case unchanged
    /// It was granted and no longer is: the person turned it off in Settings (or a policy now applies).
    case revoked
    /// It was not granted and now is, typically on return from Settings.
    case restored
}

public struct PermissionEntry: Equatable, Sendable {
    public let kind: PermissionKind
    public let status: PermissionStatus
    public let change: PermissionChange

    public init(kind: PermissionKind, status: PermissionStatus, change: PermissionChange) {
        self.kind = kind
        self.status = status
        self.change = change
    }
}

/// Reads one permission without ever asking.
public protocol PermissionStatusReading: Sendable {
    func status(of kind: PermissionKind) async -> PermissionStatus
}

/// The status each permission had when it was last read, so a revocation or a return from Settings is noticed even if it
/// happened while the app was not running. It holds no content.
public protocol PermissionMemory: Sendable {
    func lastStatus(of kind: PermissionKind) -> PermissionStatus?
    func record(_ status: PermissionStatus, for kind: PermissionKind)
}

public final class InMemoryPermissionMemory: PermissionMemory, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [PermissionKind: PermissionStatus]

    public init(initial: [PermissionKind: PermissionStatus] = [:]) {
        stored = initial
    }

    public func lastStatus(of kind: PermissionKind) -> PermissionStatus? {
        lock.lock()
        defer { lock.unlock() }
        return stored[kind]
    }

    public func record(_ status: PermissionStatus, for kind: PermissionKind) {
        lock.lock()
        defer { lock.unlock() }
        stored[kind] = status
    }
}

public final class UserDefaultsPermissionMemory: PermissionMemory, @unchecked Sendable {
    private static let keyPrefix = "com.boldfield.ohand.permission.last-status."
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func lastStatus(of kind: PermissionKind) -> PermissionStatus? {
        switch defaults.string(forKey: Self.keyPrefix + kind.rawValue) {
        case "not_requested": return .notRequested
        case "granted": return .granted
        case "denied": return .denied
        case "restricted": return .restricted
        default: return nil
        }
    }

    public func record(_ status: PermissionStatus, for kind: PermissionKind) {
        let value: String
        switch status {
        case .notRequested: value = "not_requested"
        case .granted: value = "granted"
        case .denied: value = "denied"
        case .restricted: value = "restricted"
        }
        defaults.set(value, forKey: Self.keyPrefix + kind.rawValue)
    }
}

/// Reads every permission and says how each moved. Reading never asks and never changes anything stored by the app other
/// than the remembered status, so it is safe on every appearance of the screen and on every return from Settings.
public actor PermissionsOverview {
    private let reader: PermissionStatusReading
    private let memory: PermissionMemory

    public init(reader: PermissionStatusReading, memory: PermissionMemory) {
        self.reader = reader
        self.memory = memory
    }

    public func refresh() async -> [PermissionEntry] {
        var entries: [PermissionEntry] = []
        for kind in PermissionKind.allCases {
            let status = await reader.status(of: kind)
            let change = Self.change(from: memory.lastStatus(of: kind), to: status)
            memory.record(status, for: kind)
            entries.append(PermissionEntry(kind: kind, status: status, change: change))
        }
        return entries
    }

    static func change(from previous: PermissionStatus?, to current: PermissionStatus) -> PermissionChange {
        guard let previous, previous != current else { return .unchanged }
        if previous.isGranted && !current.isGranted { return .revoked }
        if !previous.isGranted && current.isGranted { return .restored }
        return .unchanged
    }
}
