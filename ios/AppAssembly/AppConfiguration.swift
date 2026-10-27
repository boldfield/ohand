import Foundation
import OhAndServices

/// The production settings the app hands to the composition root. It names where the stores live and which route
/// captures are stamped with; the root picks every adapter itself.
enum AppConfiguration {
    /// The single universal route a fresh install has (`docs/architecture/m1-contracts.md`, routes).
    static let universalRouteID = "universal"

    static func productionStorageRoot(fileManager: FileManager = .default) throws -> URL {
        let supportDirectory = try fileManager.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        return supportDirectory
            .appendingPathComponent("OhAnd", isDirectory: true)
            .appendingPathComponent("Store", isDirectory: true)
    }

    static func production(storageRoot: URL) -> CompositionConfiguration {
        CompositionConfiguration(storageRootPath: storageRoot.path, captureRouteID: universalRouteID)
    }

    #if DEBUG
    /// The settings of a UI-test fixture launch: a fresh temporary store and a credential service name made for this
    /// launch, so nothing a fixture does can reach the production store or the Keychain items production uses.
    static func uiTestFixture(storageRoot: URL) -> CompositionConfiguration {
        CompositionConfiguration(
            storageRootPath: storageRoot.path, captureRouteID: universalRouteID,
            credentialServiceName: "com.boldfield.ohand.uitest-fixture.\(UUID().uuidString)",
            jobRetryDelaySeconds: 3600)
    }
    #endif
}
