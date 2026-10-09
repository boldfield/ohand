import Foundation

/// Every M1 location that holds captured content, derived data or temporary files.
enum ProtectedStore: String, CaseIterable, Equatable {
    case ingressStagingRecords
    case ingressInProgressAudio
    case database
    case finalizedAudio
    case searchIndexAndCaches
    case configuration
    case temporaryFiles
}

enum BackupInclusion: Equatable {
    case included
    case excluded
}

/// Coarse device lock states the contract reasons about. A simulator never locks, so these describe the
/// documented behavior of each file protection class, not something the simulator can demonstrate.
enum DeviceLockState: CaseIterable, Equatable {
    case beforeFirstUnlock
    case lockedAfterFirstUnlock
    case unlocked
}

struct LockStateAccess: Equatable {
    let canOpenExistingFile: Bool
    let canCreateNewFile: Bool
    let canKeepWritingOpenFile: Bool

    static let everything = LockStateAccess(canOpenExistingFile: true, canCreateNewFile: true, canKeepWritingOpenFile: true)
    static let nothing = LockStateAccess(canOpenExistingFile: false, canCreateNewFile: false, canKeepWritingOpenFile: false)
}

/// The settled protection class and backup treatment of one store, from "Data Protection and Backup Lifecycle" in
/// `docs/architecture/m1-contracts.md`. There is deliberately no unprotected class.
struct StoreProtectionPolicy: Equatable {
    let store: ProtectedStore
    let directoryName: String
    let fileProtection: FileProtectionType
    let backup: BackupInclusion

    /// What the file class allows in each lock state. Before the first unlock after a restart no store is available,
    /// so capture cannot start and the native surface says so instead of losing input.
    func access(in lockState: DeviceLockState) -> LockStateAccess {
        if lockState == .unlocked {
            return .everything
        }
        if lockState == .beforeFirstUnlock {
            return .nothing
        }
        if fileProtection == .completeUntilFirstUserAuthentication {
            return .everything
        }
        if fileProtection == .completeUnlessOpen {
            return LockStateAccess(canOpenExistingFile: false, canCreateNewFile: true, canKeepWritingOpenFile: true)
        }
        return .nothing
    }

    static var all: [StoreProtectionPolicy] {
        ProtectedStore.allCases.map { policy(for: $0) }
    }

    static func policy(for store: ProtectedStore) -> StoreProtectionPolicy {
        switch store {
        case .ingressStagingRecords:
            return StoreProtectionPolicy(
                store: store, directoryName: "IngressRecords",
                fileProtection: .completeUnlessOpen, backup: .excluded)
        case .ingressInProgressAudio:
            return StoreProtectionPolicy(
                store: store, directoryName: "IngressAudio",
                fileProtection: .completeUnlessOpen, backup: .excluded)
        case .database:
            return StoreProtectionPolicy(
                store: store, directoryName: "Database",
                fileProtection: .completeUntilFirstUserAuthentication, backup: .included)
        case .finalizedAudio:
            return StoreProtectionPolicy(
                store: store, directoryName: "FinalizedAudio",
                fileProtection: .completeUntilFirstUserAuthentication, backup: .excluded)
        case .searchIndexAndCaches:
            return StoreProtectionPolicy(
                store: store, directoryName: "Index",
                fileProtection: .completeUntilFirstUserAuthentication, backup: .excluded)
        case .configuration:
            return StoreProtectionPolicy(
                store: store, directoryName: "Configuration",
                fileProtection: .completeUntilFirstUserAuthentication, backup: .included)
        case .temporaryFiles:
            return StoreProtectionPolicy(
                store: store, directoryName: "Temporary",
                fileProtection: .completeUntilFirstUserAuthentication, backup: .excluded)
        }
    }
}

/// Where each store lives under one root. Stores are flat siblings so that no unmanaged intermediate directory
/// exists. SQLite's write-ahead log, shared-memory and journal files are created next to the database and inherit
/// the `Database` directory's class; `ProtectedStorageService` also reapplies it to files already present.
struct ProtectedStorageLayout: Equatable {
    static let databaseFileName = "core.sqlite"

    let rootDirectory: URL

    func directory(for store: ProtectedStore) -> URL {
        rootDirectory.appendingPathComponent(
            StoreProtectionPolicy.policy(for: store).directoryName, isDirectory: true)
    }

    var databaseURL: URL {
        directory(for: .database).appendingPathComponent(Self.databaseFileName)
    }
}
