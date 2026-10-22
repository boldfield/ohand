import XCTest
import Speech
@testable import OhAndServices

private final class ScriptedPermissionReader: PermissionStatusReading, @unchecked Sendable {
    private let lock = NSLock()
    private var statuses: [PermissionKind: PermissionStatus]

    init(_ statuses: [PermissionKind: PermissionStatus]) {
        self.statuses = statuses
    }

    func set(_ status: PermissionStatus, for kind: PermissionKind) {
        lock.lock(); defer { lock.unlock() }
        statuses[kind] = status
    }

    func status(of kind: PermissionKind) async -> PermissionStatus {
        lock.lock(); defer { lock.unlock() }
        return statuses[kind] ?? .notRequested
    }
}

final class PermissionsOverviewTests: XCTestCase {
    private func everything(_ status: PermissionStatus) -> [PermissionKind: PermissionStatus] {
        Dictionary(uniqueKeysWithValues: PermissionKind.allCases.map { ($0, status) })
    }

    private func change(_ entries: [PermissionEntry], _ kind: PermissionKind) -> PermissionChange? {
        entries.first { $0.kind == kind }?.change
    }

    func testFreshInstallReportsEveryPermissionNotRequestedAndUnchanged() async {
        let overview = PermissionsOverview(
            reader: ScriptedPermissionReader(everything(.notRequested)), memory: InMemoryPermissionMemory())
        let entries = await overview.refresh()
        XCTAssertEqual(entries.map(\.kind), PermissionKind.allCases)
        XCTAssertTrue(entries.allSatisfy { $0.status == .notRequested && $0.change == .unchanged })
    }

    func testDenialIsReportedAsDeniedWithoutBeingARevocation() async {
        let reader = ScriptedPermissionReader(everything(.notRequested))
        let overview = PermissionsOverview(reader: reader, memory: InMemoryPermissionMemory())
        _ = await overview.refresh()
        reader.set(.denied, for: .microphone)
        let entries = await overview.refresh()
        XCTAssertEqual(entries.first { $0.kind == .microphone }?.status, .denied)
        XCTAssertEqual(change(entries, .microphone), .unchanged, "never granted, so nothing was revoked")
    }

    func testRestrictedIsReportedAsRestricted() async {
        let reader = ScriptedPermissionReader(everything(.notRequested))
        reader.set(.restricted, for: .speech)
        let entries = await PermissionsOverview(reader: reader, memory: InMemoryPermissionMemory()).refresh()
        XCTAssertEqual(entries.first { $0.kind == .speech }?.status, .restricted)
    }

    func testLaterRevocationIsReportedOnceAndSettingsReturnIsReportedAsRestored() async {
        let reader = ScriptedPermissionReader(everything(.granted))
        let overview = PermissionsOverview(reader: reader, memory: InMemoryPermissionMemory())
        _ = await overview.refresh()

        reader.set(.denied, for: .notifications)
        let revoked = await overview.refresh()
        XCTAssertEqual(change(revoked, .notifications), .revoked)
        XCTAssertEqual(change(revoked, .microphone), .unchanged)

        let unchanged = await overview.refresh()
        XCTAssertEqual(change(unchanged, .notifications), .unchanged, "a revocation is called out once")

        reader.set(.granted, for: .notifications)
        let restored = await overview.refresh()
        XCTAssertEqual(change(restored, .notifications), .restored)
    }

    func testRevocationWhileTheAppWasNotRunningIsNoticedFromRememberedStatus() async {
        let memory = InMemoryPermissionMemory()
        let reader = ScriptedPermissionReader(everything(.granted))
        _ = await PermissionsOverview(reader: reader, memory: memory).refresh()

        reader.set(.restricted, for: .speech)
        let relaunched = await PermissionsOverview(reader: reader, memory: memory).refresh()
        XCTAssertEqual(change(relaunched, .speech), .revoked)
    }

    func testRememberedStatusSurvivesInUserDefaults() {
        let suite = "ohand.tests.permission-memory.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let memory = UserDefaultsPermissionMemory(defaults: defaults)
        XCTAssertNil(memory.lastStatus(of: .microphone))
        for status in [PermissionStatus.notRequested, .granted, .denied, .restricted] {
            memory.record(status, for: .microphone)
            XCTAssertEqual(UserDefaultsPermissionMemory(defaults: defaults).lastStatus(of: .microphone), status)
        }
    }

    func testSystemStatusMappingsCoverEveryFrameworkCase() {
        XCTAssertEqual(SystemPermissionStatusReader.speechStatus(.authorized), .granted)
        XCTAssertEqual(SystemPermissionStatusReader.speechStatus(.denied), .denied)
        XCTAssertEqual(SystemPermissionStatusReader.speechStatus(.restricted), .restricted)
        XCTAssertEqual(SystemPermissionStatusReader.speechStatus(.notDetermined), .notRequested)
        XCTAssertEqual(SystemPermissionStatusReader.notificationStatus(.authorized), .granted)
        XCTAssertEqual(SystemPermissionStatusReader.notificationStatus(.provisional), .granted)
        XCTAssertEqual(SystemPermissionStatusReader.notificationStatus(.ephemeral), .granted)
        XCTAssertEqual(SystemPermissionStatusReader.notificationStatus(.denied), .denied)
        XCTAssertEqual(SystemPermissionStatusReader.notificationStatus(.notDetermined), .notRequested)
    }

    func testReaderReadsNotificationStatusFromTheCenterWithoutAsking() async {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .denied
        let reader = SystemPermissionStatusReader(notifications: center)
        let status = await reader.status(of: .notifications)
        XCTAssertEqual(status, .denied)
    }

    func testGuidanceExplainsRecoveryAndWhatStillWorksForEveryState() {
        for kind in PermissionKind.allCases {
            for status in [PermissionStatus.notRequested, .granted, .denied, .restricted] {
                let guidance = PermissionGuidance.make(kind: kind, status: status)
                XCTAssertFalse(guidance.headline.isEmpty)
                XCTAssertFalse(guidance.detail.isEmpty)
                switch status {
                case .denied:
                    XCTAssertEqual(guidance.recovery, .openSettings)
                    XCTAssertTrue(guidance.detail.contains("iOS Settings"), "\(kind) denial says where to fix it")
                case .restricted:
                    XCTAssertEqual(guidance.recovery, .managedByDevice)
                    XCTAssertFalse(guidance.detail.contains("allow"), "\(kind) restriction does not offer a switch")
                    XCTAssertTrue(guidance.detail.contains("Capture still works"))
                case .granted, .notRequested:
                    XCTAssertEqual(guidance.recovery, PermissionRecovery.none)
                }
            }
        }
    }

    func testGuidanceNamesRevocationAndPromisesCaptureAndNoRemoteTranscription() {
        XCTAssertEqual(
            PermissionGuidance.make(kind: .notifications, status: .denied, change: .revoked).headline,
            "Notifications was turned off")
        XCTAssertEqual(
            PermissionGuidance.make(kind: .microphone, status: .granted, change: .restored).headline,
            "Microphone is on again")
        let speech = PermissionGuidance.make(kind: .speech, status: .denied).detail
        XCTAssertTrue(speech.contains("still saved as audio"))
        XCTAssertTrue(speech.contains("nothing is sent anywhere to be transcribed"))
        let notifications = PermissionGuidance.make(kind: .notifications, status: .denied).detail
        XCTAssertTrue(notifications.contains("saved and searchable"))
    }
}
