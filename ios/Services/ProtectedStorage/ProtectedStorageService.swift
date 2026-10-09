import Foundation

enum StorageSetupStep: String, Equatable {
    case createDirectory
    case applyBackupPolicy
    case applyProtection
    case listExistingItems
    case applyProtectionToExistingItem
}

/// One attribute that could not be applied. Carries the Foundation error domain and code only, never a message, so
/// logging a report cannot disclose content.
struct StorageSetupFailure: Equatable {
    let store: ProtectedStore
    let step: StorageSetupStep
    let url: URL
    let errorDomain: String
    let errorCode: Int
}

struct StoreSetupOutcome: Equatable {
    let store: ProtectedStore
    let directory: URL
    /// False only when the directory could not be created; nothing can be persisted there until it can.
    let directoryAvailable: Bool
    let failures: [StorageSetupFailure]

    var isFullyConfigured: Bool { directoryAvailable && failures.isEmpty }
}

struct ProtectionSetupReport: Equatable {
    let outcomes: [StoreSetupOutcome]

    var failures: [StorageSetupFailure] { outcomes.flatMap { $0.failures } }
    var isFullyConfigured: Bool { outcomes.allSatisfy { $0.isFullyConfigured } }

    func outcome(for store: ProtectedStore) -> StoreSetupOutcome? {
        outcomes.first { $0.store == store }
    }
}

/// Applies the settled file protection class and backup treatment to every store directory and to the items already
/// inside it. Setup is idempotent and safe on every launch.
///
/// A failure is recorded in the report and never stops the remaining stores or steps, and it never removes, moves or
/// rewrites stored content: a store whose class could not be applied keeps its data and keeps the class it inherited,
/// and the caller decides what to surface. Exclusion from backup is attempted before the protection class so that a
/// protection failure cannot also leave private audio or staging data eligible for backup.
final class ProtectedStorageService {
    let layout: ProtectedStorageLayout
    private let boundary: FileAttributeBoundary

    init(layout: ProtectedStorageLayout, boundary: FileAttributeBoundary = FileManagerAttributeBoundary()) {
        self.layout = layout
        self.boundary = boundary
    }

    func prepare() -> ProtectionSetupReport {
        ProtectionSetupReport(outcomes: ProtectedStore.allCases.map { applyPolicy(to: $0) })
    }

    /// Reapplies one store's policy, including to items created since the last call (for example the write-ahead log
    /// SQLite creates once the core has opened the database).
    func applyPolicy(to store: ProtectedStore) -> StoreSetupOutcome {
        let policy = StoreProtectionPolicy.policy(for: store)
        let directory = layout.directory(for: store)
        var failures: [StorageSetupFailure] = []

        func record(_ step: StorageSetupStep, _ url: URL, _ error: Error) {
            let nsError = error as NSError
            failures.append(StorageSetupFailure(
                store: store, step: step, url: url, errorDomain: nsError.domain, errorCode: nsError.code))
        }

        do {
            try boundary.createDirectory(at: directory)
        } catch {
            record(.createDirectory, directory, error)
            return StoreSetupOutcome(store: store, directory: directory, directoryAvailable: false, failures: failures)
        }

        do {
            try boundary.setExcludedFromBackup(policy.backup == .excluded, at: directory)
        } catch {
            record(.applyBackupPolicy, directory, error)
        }

        do {
            try boundary.setProtection(policy.fileProtection, at: directory)
        } catch {
            record(.applyProtection, directory, error)
        }

        do {
            for item in try boundary.descendants(of: directory) {
                do {
                    try boundary.setProtection(policy.fileProtection, at: item)
                } catch {
                    record(.applyProtectionToExistingItem, item, error)
                }
            }
        } catch {
            record(.listExistingItems, directory, error)
        }

        return StoreSetupOutcome(store: store, directory: directory, directoryAvailable: true, failures: failures)
    }
}
