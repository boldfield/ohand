import Foundation
import XCTest
@testable import OhAndServices

final class PendingProcessingAlertTests: XCTestCase {
    private let reference = Date(timeIntervalSince1970: 1_800_000_000)

    private func makeAlert(_ center: FakeNotificationCenter, delay: TimeInterval = 300) throws -> PendingProcessingAlert {
        let policy = try XCTUnwrap(PendingProcessingAlertPolicy(delay: delay))
        let instant = reference
        return PendingProcessingAlert(center: center, policy: policy, now: { instant })
    }

    func testPolicyBoundsTheDelayOnBothSides() {
        XCTAssertNil(PendingProcessingAlertPolicy(delay: 59))
        XCTAssertNil(PendingProcessingAlertPolicy(delay: 3601))
        XCTAssertNotNil(PendingProcessingAlertPolicy(delay: 60))
        XCTAssertNotNil(PendingProcessingAlertPolicy(delay: 3600))
    }

    func testArmingInstallsOneGenericOneShotRequestDueAfterTheDelay() async throws {
        let center = FakeNotificationCenter()
        let alert = try makeAlert(center, delay: 300)
        let result = await alert.arm()

        XCTAssertEqual(result, .armed(dueInstant: reference.addingTimeInterval(300)))
        let request = try XCTUnwrap(center.addedRequests.first)
        XCTAssertEqual(center.addedRequests.count, 1)
        XCTAssertEqual(request.identifier, PendingProcessingAlert.identifier)
        XCTAssertEqual(request.dueInstant, reference.addingTimeInterval(300))
        XCTAssertEqual(request.content.userInfo, [NotificationContent.kindKey: PendingProcessingAlert.kindValue])
        XCTAssertNil(request.content.userInfo[NotificationContent.targetKey], "it names no item")
    }

    func testTheAlertIsNeverMistakenForAReminderAndNeverWordedAsOne() throws {
        XCTAssertNil(NotificationIdentifier(rawValue: PendingProcessingAlert.identifier))
        XCTAssertNotEqual(PendingProcessingAlert.kindValue, NotificationPayloadKind.generic.rawValue)
        XCTAssertNotEqual(PendingProcessingAlertWording.body, GenericNotificationWording.body)
        XCTAssertTrue(PendingProcessingAlertWording.body.contains("no reminder from it is set"))
    }

    func testArmingAgainReplacesTheRequestSoAtMostOneIsEverPending() async throws {
        let center = FakeNotificationCenter()
        let alert = try makeAlert(center)
        _ = await alert.arm()
        _ = await alert.arm()

        let pending = try await center.pendingRequests()
        XCTAssertEqual(pending.map(\.identifier), [PendingProcessingAlert.identifier])
    }

    func testWithoutPermissionNothingIsInstalledOrRequested() async throws {
        for status in [NotificationAuthorization.denied, .notDetermined] {
            let center = FakeNotificationCenter()
            center.authorizationStatus = status
            let alert = try makeAlert(center)
            let result = await alert.arm()

            XCTAssertEqual(result, .notPermitted)
            XCTAssertTrue(center.addedRequests.isEmpty)
        }
    }

    func testAnInstallTheSystemDoesNotKeepIsAFailureNotAnArmedAlert() async throws {
        let center = FakeNotificationCenter()
        center.retainsAddedRequests = false
        let droppedAlert = try makeAlert(center)
        let dropped = await droppedAlert.arm()
        XCTAssertEqual(dropped, .failed)

        let failing = FakeNotificationCenter()
        failing.addError = NotificationBridgeError.installNotConfirmed
        let rejectingAlert = try makeAlert(failing)
        let rejected = await rejectingAlert.arm()
        XCTAssertEqual(rejected, .failed)
        XCTAssertEqual(failing.addedRequests.count, 0, "a failed install is not retried")
    }

    func testDisarmRemovesOnlyTheAlertAndIsIdempotent() async throws {
        let center = FakeNotificationCenter()
        let alert = try makeAlert(center)
        _ = await alert.arm()
        await alert.disarm()
        await alert.disarm()

        let pending = try await center.pendingRequests()
        XCTAssertTrue(pending.isEmpty)
        XCTAssertEqual(Set(center.removedIdentifiers), [PendingProcessingAlert.identifier])
    }
}
