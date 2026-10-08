import Foundation
import Security
import UIKit

struct KeychainSelfTestStep {
    let label: String
    let passed: Bool
    let detail: String
}

enum KeychainSelfTest {
    static let launchArgument = "-runKeychainSelfTest"

    static func run() -> [KeychainSelfTestStep] {
        var steps: [KeychainSelfTestStep] = []
        KeychainTester.deleteAllTestCredentials()

        for accessibility in KeychainTester.AccessibilityClass.allCases {
            let name = accessibility.displayName
            let expected = KeychainTester.syntheticValue(for: accessibility)

            let firstStore = KeychainTester.storeCredential(value: expected, accessibility: accessibility)
            steps.append(statusStep("\(name) store", status: firstStore, expected: errSecSuccess))
            steps.append(retrieveStep("\(name) retrieve", accessibility: accessibility, expectedValue: expected))

            let secondStore = KeychainTester.storeCredential(value: expected, accessibility: accessibility)
            steps.append(statusStep("\(name) repeated store", status: secondStore, expected: errSecSuccess))
            steps.append(retrieveStep("\(name) retrieve after repeated store", accessibility: accessibility, expectedValue: expected))
        }

        let decoyValue = "synthetic-decoy-value"
        let decoyAccessibility = KeychainTester.AccessibilityClass.afterFirstUnlockThisDeviceOnly
        let decoyStore = KeychainTester.storeCredential(
            value: decoyValue,
            accessibility: decoyAccessibility,
            service: KeychainTester.decoyServiceName
        )
        steps.append(statusStep("decoy service store", status: decoyStore, expected: errSecSuccess))

        let deleteStatuses = KeychainTester.deleteAllTestCredentials()
        steps.append(KeychainSelfTestStep(
            label: "delete all probe credentials",
            passed: deleteStatuses.allSatisfy { $0 == errSecSuccess },
            detail: deleteStatuses.map { KeychainTester.statusText($0) }.joined(separator: ",")
        ))
        for accessibility in KeychainTester.AccessibilityClass.allCases {
            let retrieved = KeychainTester.retrieveCredential(accessibility: accessibility)
            steps.append(statusStep(
                "\(accessibility.displayName) absent after delete",
                status: retrieved.status,
                expected: errSecItemNotFound
            ))
        }
        steps.append(retrieveStep(
            "decoy service untouched by probe delete",
            accessibility: decoyAccessibility,
            expectedValue: decoyValue,
            service: KeychainTester.decoyServiceName
        ))

        let decoyDelete = KeychainTester.deleteCredential(
            service: KeychainTester.decoyServiceName,
            account: KeychainTester.accountName(for: decoyAccessibility)
        )
        steps.append(statusStep("decoy service cleanup", status: decoyDelete, expected: errSecSuccess))
        return steps
    }

    static func reportLines(for steps: [KeychainSelfTestStep], protectedDataAvailable: Bool) -> [String] {
        var lines = ["KEYCHAIN_SELFTEST_ENV protectedDataAvailable=\(protectedDataAvailable)"]
        for step in steps {
            lines.append("KEYCHAIN_SELFTEST step=\"\(step.label)\" result=\(step.passed ? "PASS" : "FAIL") detail=\(step.detail)")
        }
        let failedCount = steps.filter { !$0.passed }.count
        let verdict = failedCount == 0 ? "PASS" : "FAIL"
        lines.append("KEYCHAIN_SELFTEST_RESULT \(verdict) passed=\(steps.count - failedCount) failed=\(failedCount)")
        return lines
    }

    private static func statusStep(_ label: String, status: OSStatus, expected: OSStatus) -> KeychainSelfTestStep {
        return KeychainSelfTestStep(
            label: label,
            passed: status == expected,
            detail: "status=\(KeychainTester.statusText(status)) expected=\(KeychainTester.statusText(expected))"
        )
    }

    private static func retrieveStep(
        _ label: String,
        accessibility: KeychainTester.AccessibilityClass,
        expectedValue: String,
        service: String = KeychainTester.syntheticServiceName
    ) -> KeychainSelfTestStep {
        let retrieved = KeychainTester.retrieveCredential(accessibility: accessibility, service: service)
        let matches = retrieved.status == errSecSuccess && retrieved.value == expectedValue
        return KeychainSelfTestStep(
            label: label,
            passed: matches,
            detail: "status=\(KeychainTester.statusText(retrieved.status)) valueMatches=\(retrieved.value == expectedValue)"
        )
    }

    static func runAndExit() {
        print("KEYCHAIN_SELFTEST_START")
        fflush(stdout)
        let steps = run()
        let lines = reportLines(for: steps, protectedDataAvailable: UIApplication.shared.isProtectedDataAvailable)
        for line in lines {
            print(line)
        }
        fflush(stdout)
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try? (lines.joined(separator: "\n") + "\n").write(
            to: documents.appendingPathComponent("keychain-selftest.log"),
            atomically: true,
            encoding: .utf8
        )
        let failed = steps.contains { !$0.passed }
        exit(failed ? 1 : 0)
    }
}
