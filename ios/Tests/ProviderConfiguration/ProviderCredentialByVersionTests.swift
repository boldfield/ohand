import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class VersionResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// A profile core that records what it was asked and answers nothing, to show a refused call never reaches the core.
private final class RecordingProfileCore: ProviderProfileCore, @unchecked Sendable {
    private let lock = NSLock()
    private var submissions = 0

    var submissionCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return submissions
    }

    func submit(
        _ command: ProviderProfileCommand,
        completion: @escaping @Sendable (Result<ProviderProfileAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        submissions += 1
        lock.unlock()
    }
}

/// The by-version credential operations of `ProviderCredentialEntry` against the real Rust core over an on-disk store:
/// the reference is resolved inside the services layer and never returned.
final class ProviderCredentialByVersionTests: XCTestCase {
    private static let routeID = "route-credential-by-version"
    private static let firstSecret = "synthetic-by-version-secret-0001"
    private static let secondSecret = "synthetic-by-version-secret-0002"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("credential-by-version-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    func testReplacingByVersionKeepsTheReferenceAndTheVersionAndReportsPresent() throws {
        let keychain = FakeKeychain()
        let root = try makeRoot(authenticated: true, keychain: keychain)
        defer { root.shutDown() }
        let entry = root.providerCredentials
        let reference = try entry.addCredential(Data(Self.firstSecret.utf8))
        let created = try run { try await root.providerConfiguration.createProfile(self.draft(reference: reference)) }

        let referenceBefore = storedReference(created.profileVersion)
        XCTAssertEqual(referenceBefore, reference)
        try run { try await entry.replaceCredential(Data(Self.secondSecret.utf8), profileVersion: created.profileVersion) }

        XCTAssertEqual(try run { try await entry.credentialStatus(profileVersion: created.profileVersion) }, .present)
        XCTAssertEqual(storedReference(created.profileVersion), referenceBefore, "the reference is unchanged")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM provider_profiles"), "1", "no new version was stored")
        XCTAssertEqual(keychain.items.values.map(\.secret), [Data(Self.secondSecret.utf8)], "replaced in place")
    }

    func testRemovingByVersionLeavesTheVersionCredentialMissingAndAlsoAffectsVersionsSharingTheReference() throws {
        let keychain = FakeKeychain()
        let credentials = CredentialService(
            serviceName: "com.boldfield.ohand.tests.credential-by-version.\(UUID().uuidString)", keychain: keychain)
        let root = try makeRoot(authenticated: true, keychain: keychain, credentials: credentials)
        defer { root.shutDown() }
        let entry = root.providerCredentials
        let reference = try entry.addCredential(Data(Self.firstSecret.utf8))
        let first = try run { try await root.providerConfiguration.createProfile(self.draft(reference: reference)) }
        let second = try run { try await root.providerConfiguration.createProfile(self.draft(reference: reference)) }
        XCTAssertNotEqual(first.profileVersion, second.profileVersion)

        try run { try await entry.removeCredential(profileVersion: first.profileVersion) }

        let outcome = try run { try await root.providerConfiguration.checkConnection(profileVersion: first.profileVersion) }
        XCTAssertEqual(outcome, .credentialMissing)
        XCTAssertEqual(try run { try await entry.credentialStatus(profileVersion: first.profileVersion) }, .absent)
        XCTAssertEqual(
            try run { try await entry.credentialStatus(profileVersion: second.profileVersion) }, .absent,
            "another stored version naming the same reference lost its secret too")
        let secondOutcome = try run {
            try await root.providerConfiguration.checkConnection(profileVersion: second.profileVersion)
        }
        XCTAssertEqual(secondOutcome, .credentialMissing)
        XCTAssertTrue(keychain.items.isEmpty)
    }

    func testAnUnknownAndAnUnboundVersionAreRefusedWithDistinctErrorsAndTouchNoSecret() throws {
        let keychain = FakeKeychain()
        let root = try makeRoot(authenticated: true, keychain: keychain)
        defer { root.shutDown() }
        let entry = root.providerCredentials
        let reference = try entry.addCredential(Data(Self.firstSecret.utf8))
        let created = try run { try await root.providerConfiguration.createProfile(self.draft(reference: reference)) }
        execute("UPDATE provider_profiles SET credential_ref = NULL WHERE profile_version = '\(created.profileVersion)'")

        func assertRefused(_ expected: ProviderConfigurationError, _ version: String) {
            XCTAssertThrowsError(try run { try await entry.credentialStatus(profileVersion: version) }) {
                XCTAssertEqual($0 as? ProviderConfigurationError, expected)
            }
            XCTAssertThrowsError(try run { try await entry.removeCredential(profileVersion: version) }) {
                XCTAssertEqual($0 as? ProviderConfigurationError, expected)
            }
            XCTAssertThrowsError(
                try run { try await entry.replaceCredential(Data(Self.secondSecret.utf8), profileVersion: version) }
            ) {
                XCTAssertEqual($0 as? ProviderConfigurationError, expected)
            }
        }
        assertRefused(.unknownProfileVersion, "no-such-version")
        assertRefused(.noCredentialBound, created.profileVersion)
        XCTAssertEqual(keychain.items.values.map(\.secret), [Data(Self.firstSecret.utf8)], "the secret is untouched")
    }

    func testACaptureOnlySessionIsRefusedBeforeTheCoreOrTheKeychainIsTouched() throws {
        let keychain = FakeKeychain()
        let core = RecordingProfileCore()
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let credentials = CredentialService(
            serviceName: "com.boldfield.ohand.tests.credential-by-version.\(UUID().uuidString)", keychain: keychain)
        let entry = ProviderCredentialEntry(store: credentials, session: session, profiles: core)

        XCTAssertThrowsError(try run { try await entry.replaceCredential(Data(Self.secondSecret.utf8), profileVersion: "v") }) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await entry.credentialStatus(profileVersion: "v") }) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await entry.removeCredential(profileVersion: "v") }) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }
        XCTAssertEqual(core.submissionCount, 0, "the core was not asked")
        XCTAssertTrue(keychain.items.isEmpty)
    }

    // MARK: helpers

    private func draft(reference: String) -> ProviderProfileDraft {
        ProviderProfileDraft(
            profileID: "profile-a", providerProtocol: "anthropic", model: "synthetic-model",
            credentialReference: reference, timeoutSeconds: 30)
    }

    private func makeRoot(
        authenticated: Bool, keychain: FakeKeychain, credentials: CredentialService? = nil
    ) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        let credentialService = credentials
            ?? CredentialService(
                serviceName: "com.boldfield.ohand.tests.credential-by-version.\(UUID().uuidString)",
                keychain: keychain)
        // The real transport resolves the stored secret before any network use, so a removed secret is reported as
        // `credentialMissing` without a request leaving the test.
        let sender = ProviderHTTPTransport(credentials: credentialService)
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                credentials: credentialService))
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = VersionResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil, Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return try XCTUnwrap(box.value, "the call never completed").get()
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func storedReference(_ profileVersion: String) -> String {
        scalar("SELECT credential_ref FROM provider_profiles WHERE profile_version = '\(profileVersion)'")
    }

    private func execute(_ sql: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        XCTAssertEqual(sqlite3_exec(database, sql, nil, nil, nil), SQLITE_OK)
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
