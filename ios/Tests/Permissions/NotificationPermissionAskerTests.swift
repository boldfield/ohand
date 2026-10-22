import XCTest
@testable import OhAndServices

private final class ScriptedAuthorizationRequester: NotificationAuthorizationRequesting, @unchecked Sendable {
    private let lock = NSLock()
    private let center: FakeNotificationCenter
    private let allows: Bool
    private var requests = 0

    init(center: FakeNotificationCenter, allows: Bool) {
        self.center = center
        self.allows = allows
    }

    var requestCount: Int {
        lock.lock(); defer { lock.unlock() }
        return requests
    }

    func requestAuthorization() async -> Bool {
        lock.lock()
        requests += 1
        lock.unlock()
        try? await Task.sleep(nanoseconds: 20_000_000)
        center.authorizationStatus = allows ? .authorized : .denied
        return allows
    }
}

final class NotificationPermissionAskerTests: XCTestCase {
    func testAsksWhenNeverRequestedAndReportsAllowed() async {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .notDetermined
        let requester = ScriptedAuthorizationRequester(center: center, allows: true)
        let outcome = await NotificationPermissionAsker(center: center, requester: requester).askIfNeverRequested()
        XCTAssertEqual(outcome, .asked(.usable))
        XCTAssertEqual(requester.requestCount, 1)
    }

    func testDenialAtThePromptIsReportedAndNotRepeated() async {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .notDetermined
        let requester = ScriptedAuthorizationRequester(center: center, allows: false)
        let asker = NotificationPermissionAsker(center: center, requester: requester)
        let first = await asker.askIfNeverRequested()
        XCTAssertEqual(first, .asked(.denied))
        let second = await asker.askIfNeverRequested()
        XCTAssertEqual(second, .alreadyDecided(.denied))
        XCTAssertEqual(requester.requestCount, 1, "a denial is respected: the prompt is not shown again")
    }

    func testNeverAsksWhenTheOSAlreadyDecided() async {
        for decided in [NotificationAuthorization.denied, .authorized, .provisional, .ephemeral] {
            let center = FakeNotificationCenter()
            center.authorizationStatus = decided
            let requester = ScriptedAuthorizationRequester(center: center, allows: true)
            let outcome = await NotificationPermissionAsker(center: center, requester: requester).askIfNeverRequested()
            XCTAssertEqual(outcome, .alreadyDecided(NotificationPermissionState(decided)))
            XCTAssertEqual(requester.requestCount, 0)
        }
    }

    func testConcurrentCallersShareOnePrompt() async {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .notDetermined
        let requester = ScriptedAuthorizationRequester(center: center, allows: true)
        let asker = NotificationPermissionAsker(center: center, requester: requester)
        async let first = asker.askIfNeverRequested()
        async let second = asker.askIfNeverRequested()
        _ = await (first, second)
        XCTAssertEqual(requester.requestCount, 1)
    }
}
