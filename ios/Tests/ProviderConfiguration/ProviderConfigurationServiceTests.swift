import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class ScriptedProfileCore: ProviderProfileCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ProviderProfileAnswer, CoreFailure>]
    private var received: [ProviderProfileCommand] = []

    init(_ script: [Result<ProviderProfileAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [ProviderProfileCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: ProviderProfileCommand,
        completion: @escaping @Sendable (Result<ProviderProfileAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(ProviderConfigurationClientFailure.unreadableAnswer))
    }
}

private final class ScriptedConnectionCheckCore: ProviderConnectionCheckCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ProviderConnectionOutcome, CoreFailure>]
    private var received: [String] = []

    init(_ script: [Result<ProviderConnectionOutcome, CoreFailure>]) {
        self.script = script
    }

    var profileVersions: [String] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func check(
        profileVersion: String,
        completion: @escaping @Sendable (Result<ProviderConnectionOutcome, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(profileVersion)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(ProviderConfigurationClientFailure.unreadableAnswer))
    }
}

private final class ScriptedRoutePolicyCore: RoutePolicyCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<RoutePolicyAnswer, CoreFailure>]
    private var received: [RoutePolicyCommand] = []
    var beforeAnswer: (@Sendable () -> Void)?

    init(_ script: [Result<RoutePolicyAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [RoutePolicyCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: RoutePolicyCommand,
        completion: @escaping @Sendable (Result<RoutePolicyAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        beforeAnswer?()
        completion(next ?? .failure(ProviderConfigurationClientFailure.unreadableAnswer))
    }
}

final class ProviderConfigurationServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func failure(_ code: String) -> CoreFailure {
        CoreFailure(errorClass: .permanent, code: code, message: "fixed message")
    }

    private func decode<Value: Decodable>(_ type: Value.Type, _ json: String) throws -> Value {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private func draft() -> ProviderProfileDraft {
        ProviderProfileDraft(
            profileID: "profile-a", providerProtocol: "anthropic", model: "synthetic-model",
            credentialReference: "synthetic-reference", timeoutSeconds: 30)
    }

    private func makeService(
        authenticated: Bool = true,
        profiles: ScriptedProfileCore = ScriptedProfileCore([]),
        checks: ScriptedConnectionCheckCore = ScriptedConnectionCheckCore([]),
        routes: ScriptedRoutePolicyCore = ScriptedRoutePolicyCore([])
    ) -> ProviderConfigurationService {
        ProviderConfigurationService(
            profiles: profiles, connectionChecks: checks, routePolicy: routes,
            session: makeSession(authenticated: authenticated))
    }

    func testEachCallSendsItsCommandAndReturnsTheTypedAnswer() async throws {
        let created = try decode(
            ProviderProfileCreatedAnswer.self,
            #"{"operation_id":1,"profile_id":"profile-a","profile_version":"version-1"}"#)
        let defaultSet = try decode(
            ProviderProfileDefaultSetAnswer.self,
            #"{"operation_id":2,"capability":"text_interpretation","profile_version":"version-1"}"#)
        let defaultCleared = try decode(
            ProviderProfileDefaultClearedAnswer.self, #"{"operation_id":3,"capability":"text_interpretation"}"#)
        let listing = try decode(
            ProviderProfileListAnswer.self, #"{"operation_id":4,"profiles":[],"defaults":[]}"#)
        let granted = try decode(RoutePolicyChangeOutcome.self, #"{"operation_id":5,"disposition":"granted"}"#)
        let revoked = try decode(RoutePolicyChangeOutcome.self, #"{"operation_id":6,"disposition":"revoked"}"#)
        let routeListing = try decode(
            RoutePolicyListing.self,
            """
            {"operation_id":7,"routes":[{"route_id":"route-a","route_name":"Capture","scope":"capture",\
            "granted_capabilities":[{"capability":"text_interpretation","destination_classes":["cloud"]}]}]}
            """)
        let profiles = ScriptedProfileCore([
            .success(.created(created)), .success(.defaultSet(defaultSet)),
            .success(.defaultCleared(defaultCleared)), .success(.listing(listing)),
        ])
        let checks = ScriptedConnectionCheckCore([.success(.credentialRejected)])
        let routes = ScriptedRoutePolicyCore([
            .success(.changed(granted)), .success(.changed(revoked)), .success(.listing(routeListing)),
        ])
        let service = makeService(profiles: profiles, checks: checks, routes: routes)

        let createdResult = try await service.createProfile(draft())
        XCTAssertEqual(createdResult, created)
        try await service.setDefault(capability: "text_interpretation", profileVersion: "version-1")
        try await service.clearDefault(capability: "text_interpretation")
        let listed = try await service.listProfiles()
        XCTAssertEqual(listed, listing)
        let outcome = try await service.checkConnection(profileVersion: "version-1")
        XCTAssertEqual(outcome, .credentialRejected)
        let grantResult = try await service.grant(
            routeID: "route-a", capability: "text_interpretation", profileVersion: "version-1")
        XCTAssertEqual(grantResult, .granted)
        let revokeResult = try await service.revoke(routeID: "route-a", capability: "text_interpretation")
        XCTAssertEqual(revokeResult, .revoked)
        let routeList = try await service.listRoutePolicy()
        XCTAssertEqual(routeList, routeListing.routes)

        XCTAssertEqual(
            profiles.commands,
            [
                .createProfile(draft()),
                .setDefault(capability: "text_interpretation", profileVersion: "version-1"),
                .clearDefault(capability: "text_interpretation"),
                .list,
            ])
        XCTAssertEqual(checks.profileVersions, ["version-1"])
        XCTAssertEqual(
            routes.commands,
            [
                .grant(routeID: "route-a", capability: "text_interpretation", profileVersion: "version-1"),
                .revoke(routeID: "route-a", capability: "text_interpretation"),
                .list,
            ])
    }

    func testACaptureOnlySessionIsRefusedBeforeTheCoreIsAsked() async throws {
        let profiles = ScriptedProfileCore([])
        let checks = ScriptedConnectionCheckCore([])
        let routes = ScriptedRoutePolicyCore([])
        let service = makeService(authenticated: false, profiles: profiles, checks: checks, routes: routes)

        func assertRefused<Value>(_ operation: () async throws -> Value, _ label: String) async {
            do {
                _ = try await operation()
                XCTFail("\(label) succeeded without an authenticated session")
            } catch {
                XCTAssertEqual(error as? ProviderConfigurationError, .notAuthenticated, label)
            }
        }

        await assertRefused({ try await service.createProfile(self.draft()) }, "createProfile")
        await assertRefused(
            { try await service.setDefault(capability: "text_interpretation", profileVersion: "v") }, "setDefault")
        await assertRefused({ try await service.clearDefault(capability: "text_interpretation") }, "clearDefault")
        await assertRefused({ try await service.listProfiles() }, "listProfiles")
        await assertRefused({ try await service.checkConnection(profileVersion: "v") }, "checkConnection")
        await assertRefused(
            { try await service.grant(routeID: "r", capability: "text_interpretation", profileVersion: "v") },
            "grant")
        await assertRefused({ try await service.revoke(routeID: "r", capability: "text_interpretation") }, "revoke")
        await assertRefused({ try await service.listRoutePolicy() }, "listRoutePolicy")

        XCTAssertTrue(profiles.commands.isEmpty)
        XCTAssertTrue(checks.profileVersions.isEmpty)
        XCTAssertTrue(routes.commands.isEmpty)
    }

    func testEmptyIdentifiersAreRefusedWithoutAskingTheCore() async {
        let profiles = ScriptedProfileCore([])
        let routes = ScriptedRoutePolicyCore([])
        let service = makeService(profiles: profiles, routes: routes)

        do {
            try await service.setDefault(capability: "", profileVersion: "v")
            XCTFail("an empty capability was accepted")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .invalidRequest)
        }
        do {
            _ = try await service.grant(routeID: "", capability: "text_interpretation", profileVersion: "v")
            XCTFail("an empty route was accepted")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .invalidRequest)
        }
        XCTAssertTrue(profiles.commands.isEmpty)
        XCTAssertTrue(routes.commands.isEmpty)
    }

    func testCoreFailuresKeepTheirFixedCodeAndAuthenticationCodesMapToNotAuthenticated() async {
        let profiles = ScriptedProfileCore([
            .failure(failure("unknown_protocol")), .failure(failure("read_relocked")),
            .failure(failure("invalid_request")),
        ])
        let service = makeService(profiles: profiles)

        do {
            _ = try await service.createProfile(draft())
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .core(failure("unknown_protocol")))
        }
        do {
            _ = try await service.createProfile(draft())
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .notAuthenticated)
        }
        do {
            _ = try await service.createProfile(draft())
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .invalidRequest)
        }
    }

    func testAnAnswerOfTheWrongKindIsUnreadable() async throws {
        let cleared = try decode(
            ProviderProfileDefaultClearedAnswer.self, #"{"operation_id":1,"capability":"text_interpretation"}"#)
        let service = makeService(profiles: ScriptedProfileCore([.success(.defaultCleared(cleared))]))

        do {
            _ = try await service.createProfile(draft())
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .unreadable)
        }
    }

    func testAListingWhoseSessionRelockedWhileItRanIsDropped() async throws {
        let session = makeSession(authenticated: true)
        let routeListing = try decode(RoutePolicyListing.self, #"{"operation_id":1,"routes":[]}"#)
        let routes = ScriptedRoutePolicyCore([.success(.listing(routeListing))])
        routes.beforeAnswer = { session.relock(.explicit) }
        let service = ProviderConfigurationService(
            profiles: ScriptedProfileCore([]), connectionChecks: ScriptedConnectionCheckCore([]), routePolicy: routes,
            session: session)

        do {
            _ = try await service.listRoutePolicy()
            XCTFail("a listing was delivered after the session relocked")
        } catch {
            XCTAssertEqual(error as? ProviderConfigurationError, .notAuthenticated)
        }
    }

    func testInFlightOperationsHandOutIdentifiersFromTheirRangeAndRefuseAfterShutDown() {
        let operations = InFlightOperations<String>(firstOperationID: 100)
        XCTAssertEqual(operations.begin("first"), 100)
        XCTAssertEqual(operations.begin("second"), 101)
        XCTAssertTrue(operations.contains(100))
        XCTAssertEqual(operations.finish(100), "first")
        XCTAssertNil(operations.finish(100))
        XCTAssertEqual(operations.shutDown(), ["second"])
        XCTAssertNil(operations.begin("late"))
        XCTAssertTrue(operations.shutDown().isEmpty)
    }
}
