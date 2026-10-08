import XCTest
import Security
@testable import OhAndServices

/// One half of the cross-process relaunch check, driven by `ios/scripts/credential-relaunch-simulator.sh`.
///
/// The script launches the hosted test bundle twice, each time as a new host app process, with
/// `TEST_RUNNER_OHAND_RELAUNCH_PHASE` set to `write` and then `verify`. The write run stores a synthetic secret
/// through `CredentialService` against the real simulator Keychain and exits; the verify run, in a different
/// process, must still resolve the same secret, read back the required accessibility class, then delete it.
/// In an ordinary `OhAndTests` run the variable is unset and this test is skipped. The simulator never locks the
/// device, so this checks process relaunch only, not lock behavior.
final class CredentialRelaunchTests: XCTestCase {
    private static let phaseVariable = "OHAND_RELAUNCH_PHASE"
    private static let serviceName = "com.boldfield.ohand.tests.credentials.relaunch"
    private static let reference = "5E1F0C7A-04D0-4C0D-9A11-000000000004"
    private static let secretPrefix = "synthetic-relaunch-credential-pid-"

    private let service = CredentialService(serviceName: CredentialRelaunchTests.serviceName)

    private func storedAccessibility() throws -> String {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.serviceName,
            kSecAttrAccount as String: Self.reference,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, &result), errSecSuccess)
        let attributes = try XCTUnwrap(result as? [String: Any])
        return try XCTUnwrap(attributes[kSecAttrAccessible as String] as? String)
    }

    func testRelaunchPhase() throws {
        guard let phase = ProcessInfo.processInfo.environment[Self.phaseVariable], !phase.isEmpty else {
            throw XCTSkip("\(Self.phaseVariable) is unset; the relaunch check runs only from credential-relaunch-simulator.sh")
        }
        let currentPid = ProcessInfo.processInfo.processIdentifier

        switch phase {
        case "write":
            try service.deleteCredential(reference: Self.reference)
            let secret = Data("\(Self.secretPrefix)\(currentPid)".utf8)
            try service.updateCredential(secret, reference: Self.reference)
            XCTAssertEqual(try service.credentialStatus(reference: Self.reference), .present)
            XCTAssertEqual(try service.resolveSecret(reference: Self.reference), secret)
            XCTAssertEqual(try storedAccessibility(), kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
            print("CREDENTIAL_RELAUNCH phase=write pid=\(currentPid) result=stored")

        case "verify":
            defer { try? service.deleteCredential(reference: Self.reference) }
            XCTAssertEqual(try service.credentialStatus(reference: Self.reference), .present)
            let secret = try service.resolveSecret(reference: Self.reference)
            let text = String(decoding: secret, as: UTF8.self)
            XCTAssertTrue(text.hasPrefix(Self.secretPrefix), "stored secret did not survive the relaunch")
            let writerPid = Int32(text.dropFirst(Self.secretPrefix.count))
            XCTAssertNotNil(writerPid)
            XCTAssertNotEqual(writerPid, currentPid, "verify must run in a different process than write")
            XCTAssertEqual(try storedAccessibility(), kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
            try service.deleteCredential(reference: Self.reference)
            XCTAssertEqual(try service.credentialStatus(reference: Self.reference), .absent)
            print("CREDENTIAL_RELAUNCH phase=verify pid=\(currentPid) writerPid=\(writerPid ?? -1) result=survived")

        default:
            XCTFail("unknown \(Self.phaseVariable) value")
        }
    }
}
