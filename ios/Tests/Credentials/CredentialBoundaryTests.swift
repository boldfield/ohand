import XCTest
import Security
@testable import OhAndServices

/// Canary tests for the secret boundary: a unique synthetic secret goes in, and nothing the service hands back to
/// the UI, core bridge, logs, exports or synced profile data may contain it in any encoding.
final class CredentialBoundaryTests: XCTestCase {
    private let canaryText = "synthetic-canary-credential-value-0042"
    private var canary: Data { Data(canaryText.utf8) }

    private func forbiddenRenderings() -> [String] {
        [
            canaryText,
            canary.base64EncodedString(),
            canary.map { String(format: "%02x", $0) }.joined(),
            canary.map { String(format: "%02X", $0) }.joined(),
        ]
    }

    private func assertNoCanary(in text: String, _ label: String, file: StaticString = #filePath, line: UInt = #line) {
        for rendering in forbiddenRenderings() {
            XCTAssertFalse(text.contains(rendering), "\(label) leaked the canary secret", file: file, line: line)
        }
    }

    private func everyRendering(of error: Error) -> [String] {
        var dumped = ""
        dump(error, to: &dumped)
        return [
            "\(error)",
            String(describing: error),
            String(reflecting: error),
            error.localizedDescription,
            (error as? LocalizedError)?.errorDescription ?? "",
            dumped,
        ]
    }

    func testNothingReturnedOrThrownByTheServiceContainsTheSecret() throws {
        let keychain = FakeKeychain()
        let service = CredentialService(serviceName: "com.boldfield.ohand.tests.credentials.canary", keychain: keychain)
        var outputs: [String] = []

        let reference = try service.addCredential(canary)
        XCTAssertTrue(keychain.items.values.contains { $0.secret == canary }, "canary must really be stored")
        outputs.append(reference)
        outputs.append(try service.credentialStatus(reference: reference).rawValue)
        let encodedStatus = try JSONEncoder().encode(try service.credentialStatus(reference: reference))
        outputs.append(String(decoding: encodedStatus, as: UTF8.self))
        try service.updateCredential(canary, reference: reference)
        outputs.append(try service.credentialStatus(reference: reference).rawValue)

        let faults: [OSStatus] = [
            errSecInteractionNotAllowed, errSecDecode, errSecAuthFailed, errSecMissingEntitlement, 424_242,
        ]
        for status in faults {
            keychain.forcedStatus = [.insert: status, .replace: status, .read: status, .remove: status]
            let failures: [() throws -> Any] = [
                { try service.addCredential(self.canary) },
                { try service.updateCredential(self.canary, reference: reference) },
                { try service.deleteCredential(reference: reference) },
                { try service.credentialStatus(reference: reference) },
                { try service.resolveSecret(reference: reference) },
            ]
            for failure in failures {
                do {
                    let value = try failure()
                    outputs.append("\(value)")
                } catch {
                    outputs.append(contentsOf: everyRendering(of: error))
                }
            }
        }
        keychain.forcedStatus = [:]

        keychain.plant(key: KeychainItemKey(service: "com.boldfield.ohand.tests.credentials.canary", account: reference), secret: Data())
        do {
            _ = try service.resolveSecret(reference: reference)
            XCTFail("an empty stored value must not resolve")
        } catch {
            outputs.append(contentsOf: everyRendering(of: error))
        }

        XCTAssertGreaterThan(outputs.count, 20)
        for output in outputs {
            assertNoCanary(in: output, "service output")
        }
    }

    func testRejectedReferenceNeverEchoesWhatWasPassedIn() {
        let service = CredentialService(serviceName: "com.boldfield.ohand.tests.credentials.canary", keychain: FakeKeychain())
        XCTAssertThrowsError(try service.updateCredential(Data("x".utf8), reference: canaryText)) { error in
            XCTAssertEqual(error as? CredentialError, .invalidReference)
            for rendering in self.everyRendering(of: error) {
                self.assertNoCanary(in: rendering, "invalid reference error")
            }
        }
    }

    func testServiceRetainsNoSecretStateOfItsOwn() throws {
        let realBackedService = CredentialService(serviceName: "com.boldfield.ohand.tests.credentials.reflect")
        var reflected = ""
        dump(realBackedService, to: &reflected)
        assertNoCanary(in: reflected, "service reflection")
    }

    func testPublicSurfaceExposesOnlyReferencesStatusAndErrors() {
        let service = CredentialService(serviceName: "com.boldfield.ohand.tests.credentials.surface", keychain: FakeKeychain())
        let add: (Data) throws -> String = service.addCredential(_:)
        let update: (Data, String) throws -> Void = service.updateCredential(_:reference:)
        let delete: (String) throws -> Void = service.deleteCredential(reference:)
        let status: (String) throws -> CredentialStatus = service.credentialStatus(reference:)
        XCTAssertNotNil(add as Any)
        XCTAssertNotNil(update as Any)
        XCTAssertNotNil(delete as Any)
        XCTAssertNotNil(status as Any)
    }

    // MARK: source-level boundary checks

    private var iosRoot: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }

    private func credentialSources() throws -> [(name: String, text: String)] {
        let directory = iosRoot.appendingPathComponent("Services/Credentials")
        let names = try FileManager.default.contentsOfDirectory(atPath: directory.path).filter { $0.hasSuffix(".swift") }
        XCTAssertFalse(names.isEmpty, "no credential sources found at \(directory.path)")
        return try names.sorted().map { name in
            (name: name, text: try String(contentsOf: directory.appendingPathComponent(name), encoding: .utf8))
        }
    }

    func testCredentialSourcesContainNoLoggingOrPrinting() throws {
        let forbiddenCalls = ["print(", "debugPrint(", "NSLog(", "os_log(", "Logger(", "dump(", "OSLog", "writeToFile", ".write(to:"]
        for source in try credentialSources() {
            for call in forbiddenCalls {
                XCTAssertFalse(source.text.contains(call), "\(source.name) uses \(call)")
            }
        }
    }

    func testSecretResolutionIsNotPublic() throws {
        let declarations = try credentialSources().flatMap { source in
            source.text.components(separatedBy: "\n").filter { $0.contains("func resolveSecret") }
        }
        XCTAssertEqual(declarations.count, 1)
        for declaration in declarations {
            XCTAssertFalse(declaration.contains("public"), "resolveSecret must stay module-internal")
            XCTAssertFalse(declaration.contains("open"), "resolveSecret must stay module-internal")
        }
    }

    func testOnlyTheDeviceOnlyAfterFirstUnlockClassIsUsedAndNoAccessGroupIsRequested() throws {
        let accessibilityNames = try credentialSources().flatMap { source -> [String] in
            let pattern = try NSRegularExpression(pattern: "kSecAttrAccessible[A-Za-z]+")
            let range = NSRange(source.text.startIndex..., in: source.text)
            return pattern.matches(in: source.text, range: range).compactMap { match in
                Range(match.range, in: source.text).map { String(source.text[$0]) }
            }
        }
        XCTAssertFalse(accessibilityNames.isEmpty)
        XCTAssertEqual(Set(accessibilityNames), ["kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly"])
        for source in try credentialSources() {
            XCTAssertFalse(source.text.contains("kSecAttrAccessGroup"), "\(source.name) must not name an access group")
            XCTAssertFalse(source.text.contains("kSecAttrSynchronizableAny"), "\(source.name) must not read synced items")
        }
    }

    func testHostEntitlementsGrantOnlyTheAppsOwnKeychainGroup() throws {
        let entitlementsURL = iosRoot.appendingPathComponent("Config/OhAndApp.entitlements")
        let data = try Data(contentsOf: entitlementsURL)
        let plist = try XCTUnwrap(try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any])
        XCTAssertEqual(plist["keychain-access-groups"] as? [String], ["$(AppIdentifierPrefix)com.boldfield.ohand.app"])
    }
}
