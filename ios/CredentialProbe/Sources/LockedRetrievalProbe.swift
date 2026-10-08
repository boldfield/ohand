import Foundation
import Security
import UIKit

/// Retrieves the synthetic credentials from a background task after the scene backgrounds
/// (for example when the device locks) and appends each attempt to a log that is readable
/// while the device is locked, so P09 can read it after unlocking.
final class LockedRetrievalProbe {
    static let shared = LockedRetrievalProbe()
    static let attemptOffsetsSeconds: [Double] = [1, 3, 6, 10, 15, 20, 25]

    private(set) var isArmed = false
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid

    var logFileURL: URL {
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return documents.appendingPathComponent("locked-retrieval.log")
    }

    func arm() {
        isArmed = true
        append("ARMED at \(Self.timestamp()); lock the device now")
    }

    func readLog() -> String {
        return (try? String(contentsOf: logFileURL, encoding: .utf8)) ?? ""
    }

    func clearLog() {
        try? FileManager.default.removeItem(at: logFileURL)
    }

    func beginIfArmed() {
        guard isArmed, backgroundTask == .invalid else { return }
        isArmed = false
        append("BACKGROUND at \(Self.timestamp())")
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "locked-retrieval") { [weak self] in
            self?.append("EXPIRED at \(Self.timestamp())")
            self?.finish()
        }
        let lastOffset = Self.attemptOffsetsSeconds.last
        for offset in Self.attemptOffsetsSeconds {
            DispatchQueue.main.asyncAfter(deadline: .now() + offset) { [weak self] in
                self?.attempt(offsetSeconds: offset)
                if offset == lastOffset {
                    self?.finish()
                }
            }
        }
    }

    private func attempt(offsetSeconds: Double) {
        guard backgroundTask != .invalid else { return }
        let application = UIApplication.shared
        let protectedData = application.isProtectedDataAvailable
        let stateName = Self.stateName(application.applicationState)
        for accessibility in KeychainTester.AccessibilityClass.allCases {
            let retrieved = KeychainTester.retrieveCredential(accessibility: accessibility)
            let matches = retrieved.value == KeychainTester.syntheticValue(for: accessibility)
            append(
                "\(Self.timestamp()) offset=\(Int(offsetSeconds))s protectedData=\(protectedData) "
                    + "appState=\(stateName) class=\(accessibility.displayName) "
                    + "status=\(KeychainTester.statusText(retrieved.status)) valueMatches=\(matches)"
            )
        }
    }

    private func finish() {
        guard backgroundTask != .invalid else { return }
        append("DONE at \(Self.timestamp())")
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    private func append(_ line: String) {
        let existing = readLog()
        let updated = existing + line + "\n"
        try? Data(updated.utf8).write(to: logFileURL, options: [.atomic, .noFileProtection])
    }

    private static func timestamp() -> String {
        return ISO8601DateFormatter().string(from: Date())
    }

    private static func stateName(_ state: UIApplication.State) -> String {
        switch state {
        case .active:
            return "active"
        case .inactive:
            return "inactive"
        case .background:
            return "background"
        @unknown default:
            return "unknown"
        }
    }
}
