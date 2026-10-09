import XCTest
@testable import OhAndServices

/// Policy and service behavior against a recording boundary. These tests check the attributes the service asks the
/// operating system to apply and the documented lock-state expectations for each class. They cannot show that a
/// physical device enforces a class: the simulator never locks, so lock behavior is device evidence, not tested here.
final class StoreProtectionPolicyTests: XCTestCase {
    private let layout = ProtectedStorageLayout(rootDirectory: URL(fileURLWithPath: "/synthetic/ohand-root", isDirectory: true))

    /// An independent copy of the contract table in `docs/architecture/m1-contracts.md` (Data Protection and Backup
    /// Lifecycle), so a change to either side must be made deliberately.
    private let contractTable: [(ProtectedStore, FileProtectionType, BackupInclusion)] = [
        (.ingressStagingRecords, .completeUnlessOpen, .excluded),
        (.ingressInProgressAudio, .completeUnlessOpen, .excluded),
        (.database, .completeUntilFirstUserAuthentication, .included),
        (.finalizedAudio, .completeUntilFirstUserAuthentication, .excluded),
        (.searchIndexAndCaches, .completeUntilFirstUserAuthentication, .excluded),
        (.configuration, .completeUntilFirstUserAuthentication, .included),
        (.temporaryFiles, .completeUntilFirstUserAuthentication, .excluded),
    ]

    func testEveryStoreMatchesTheContractTable() {
        XCTAssertEqual(Set(contractTable.map { $0.0 }), Set(ProtectedStore.allCases), "every store has a contract row")
        for (store, protection, backup) in contractTable {
            let policy = StoreProtectionPolicy.policy(for: store)
            XCTAssertEqual(policy.fileProtection, protection, "\(store) protection class")
            XCTAssertEqual(policy.backup, backup, "\(store) backup treatment")
        }
    }

    func testNoStoreIsUnprotected() {
        for policy in StoreProtectionPolicy.all {
            XCTAssertNotEqual(policy.fileProtection, FileProtectionType.none, "\(policy.store) must not use an unprotected class")
            XCTAssertNotEqual(policy.fileProtection, .complete, "\(policy.store) must stay writable after first unlock")
        }
    }

    func testStoreDirectoriesAreDistinctSiblingsOfTheRoot() {
        let directories = ProtectedStore.allCases.map { layout.directory(for: $0) }
        XCTAssertEqual(Set(directories.map { $0.path }).count, ProtectedStore.allCases.count)
        for directory in directories {
            XCTAssertEqual(directory.deletingLastPathComponent().path, layout.rootDirectory.path)
        }
        XCTAssertEqual(layout.databaseURL.deletingLastPathComponent().path, layout.directory(for: .database).path)
    }

    // MARK: Lock-state expectations

    func testUntilFirstAuthenticationStoresAreUnavailableOnlyBeforeFirstUnlock() {
        for store in [ProtectedStore.database, .finalizedAudio, .searchIndexAndCaches, .configuration, .temporaryFiles] {
            let policy = StoreProtectionPolicy.policy(for: store)
            XCTAssertEqual(policy.access(in: .beforeFirstUnlock), .nothing, "\(store)")
            XCTAssertEqual(policy.access(in: .lockedAfterFirstUnlock), .everything, "\(store)")
            XCTAssertEqual(policy.access(in: .unlocked), .everything, "\(store)")
        }
    }

    func testStagingStoresAcceptNewFilesWhileLockedButCannotReopenThem() {
        for store in [ProtectedStore.ingressStagingRecords, .ingressInProgressAudio] {
            let policy = StoreProtectionPolicy.policy(for: store)
            let locked = policy.access(in: .lockedAfterFirstUnlock)
            XCTAssertTrue(locked.canCreateNewFile, "\(store) a locked handoff stays durable")
            XCTAssertTrue(locked.canKeepWritingOpenFile, "\(store) an open file keeps writing")
            XCTAssertFalse(locked.canOpenExistingFile, "\(store) a file closed while locked waits for the next unlock")
            XCTAssertEqual(policy.access(in: .beforeFirstUnlock), .nothing, "\(store) capture cannot start before first unlock")
            XCTAssertEqual(policy.access(in: .unlocked), .everything, "\(store)")
        }
    }

    // MARK: Service behavior

    func testPrepareAppliesClassAndBackupTreatmentToEveryStoreDirectory() {
        let boundary = RecordingFileAttributeBoundary()
        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        XCTAssertTrue(report.isFullyConfigured)
        XCTAssertEqual(report.outcomes.map { $0.store }, ProtectedStore.allCases)
        for (store, protection, backup) in contractTable {
            let path = layout.directory(for: store).path
            XCTAssertTrue(boundary.calls.contains(.createDirectory(path)), "\(store) directory is created")
            XCTAssertEqual(boundary.protectionByPath[path], protection, "\(store)")
            XCTAssertEqual(boundary.exclusionByPath[path], backup == .excluded, "\(store)")
        }
    }

    func testExclusionIsAppliedBeforeProtectionForEachStore() {
        let boundary = RecordingFileAttributeBoundary()
        _ = ProtectedStorageService(layout: layout, boundary: boundary).prepare()
        let path = layout.directory(for: .finalizedAudio).path
        let exclusionIndex = boundary.calls.firstIndex { if case .setExcludedFromBackup(_, path) = $0 { return true } else { return false } }
        let protectionIndex = boundary.calls.firstIndex { if case .setProtection(_, path) = $0 { return true } else { return false } }
        XCTAssertNotNil(exclusionIndex)
        XCTAssertNotNil(protectionIndex)
        if let exclusionIndex, let protectionIndex {
            XCTAssertLessThan(exclusionIndex, protectionIndex)
        }
    }

    func testItemsAlreadyInAStoreReceiveThatStoresClass() {
        let boundary = RecordingFileAttributeBoundary()
        let databaseDirectory = layout.directory(for: .database)
        let stagingDirectory = layout.directory(for: .ingressStagingRecords)
        let databaseFiles = ["core.sqlite", "core.sqlite-wal", "core.sqlite-shm", "core.sqlite-journal"].map {
            databaseDirectory.appendingPathComponent($0)
        }
        let stagedRecord = stagingDirectory.appendingPathComponent("capture-1.json")
        boundary.simulatedItems = databaseFiles + [stagedRecord]

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        XCTAssertTrue(report.isFullyConfigured)
        for file in databaseFiles {
            XCTAssertEqual(boundary.protectionByPath[file.path], .completeUntilFirstUserAuthentication, file.lastPathComponent)
        }
        XCTAssertEqual(boundary.protectionByPath[stagedRecord.path], .completeUnlessOpen)
    }

    func testPrepareIsIdempotent() {
        let boundary = RecordingFileAttributeBoundary()
        let service = ProtectedStorageService(layout: layout, boundary: boundary)
        let first = service.prepare()
        let protectionAfterFirst = boundary.protectionByPath
        let exclusionAfterFirst = boundary.exclusionByPath
        let second = service.prepare()
        XCTAssertEqual(first, second)
        XCTAssertEqual(boundary.protectionByPath, protectionAfterFirst)
        XCTAssertEqual(boundary.exclusionByPath, exclusionAfterFirst)
    }

    func testProtectionFailureIsReportedAndOtherStoresStillConfigured() {
        let boundary = RecordingFileAttributeBoundary()
        let failingPath = layout.directory(for: .database).path
        boundary.shouldFail = { call in
            if case .setProtection(_, failingPath) = call { return true }
            return false
        }

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        XCTAssertFalse(report.isFullyConfigured)
        let database = report.outcome(for: .database)
        XCTAssertEqual(database?.directoryAvailable, true, "the directory and its data remain usable")
        XCTAssertEqual(database?.failures.map { $0.step }, [.applyProtection])
        XCTAssertEqual(database?.failures.first?.errorDomain, "synthetic.protected-storage")
        XCTAssertEqual(boundary.exclusionByPath[failingPath], false, "the backup treatment was still applied")
        for store in ProtectedStore.allCases where store != .database {
            XCTAssertEqual(report.outcome(for: store)?.isFullyConfigured, true, "\(store)")
        }
    }

    func testBackupExclusionFailureStillAppliesProtection() {
        let boundary = RecordingFileAttributeBoundary()
        let audioPath = layout.directory(for: .finalizedAudio).path
        boundary.shouldFail = { call in
            if case .setExcludedFromBackup(_, audioPath) = call { return true }
            return false
        }

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        let audio = report.outcome(for: .finalizedAudio)
        XCTAssertEqual(audio?.failures.map { $0.step }, [.applyBackupPolicy])
        XCTAssertEqual(boundary.protectionByPath[audioPath], .completeUntilFirstUserAuthentication)
    }

    func testDirectoryCreationFailureMarksOnlyThatStoreUnavailable() {
        let boundary = RecordingFileAttributeBoundary()
        let blockedPath = layout.directory(for: .configuration).path
        boundary.shouldFail = { $0 == .createDirectory(blockedPath) }

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        let configuration = report.outcome(for: .configuration)
        XCTAssertEqual(configuration?.directoryAvailable, false)
        XCTAssertEqual(configuration?.failures.map { $0.step }, [.createDirectory])
        XCTAssertNil(boundary.protectionByPath[blockedPath], "no attribute is applied to a directory that was not created")
        XCTAssertEqual(report.outcomes.filter { $0.isFullyConfigured }.count, ProtectedStore.allCases.count - 1)
    }

    func testFailureOnOneExistingItemDoesNotSkipTheRest() {
        let boundary = RecordingFileAttributeBoundary()
        let directory = layout.directory(for: .finalizedAudio)
        let first = directory.appendingPathComponent("a.m4a")
        let second = directory.appendingPathComponent("b.m4a")
        boundary.simulatedItems = [first, second]
        boundary.shouldFail = { $0 == .setProtection(.completeUntilFirstUserAuthentication, first.path) }

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        XCTAssertEqual(report.outcome(for: .finalizedAudio)?.failures.map { $0.step }, [.applyProtectionToExistingItem])
        XCTAssertEqual(report.outcome(for: .finalizedAudio)?.failures.first?.url, first)
        XCTAssertEqual(boundary.protectionByPath[second.path], .completeUntilFirstUserAuthentication)
    }

    func testListingFailureIsReported() {
        let boundary = RecordingFileAttributeBoundary()
        let path = layout.directory(for: .searchIndexAndCaches).path
        boundary.shouldFail = { $0 == .descendants(path) }

        let report = ProtectedStorageService(layout: layout, boundary: boundary).prepare()

        XCTAssertEqual(report.outcome(for: .searchIndexAndCaches)?.failures.map { $0.step }, [.listExistingItems])
    }
}
