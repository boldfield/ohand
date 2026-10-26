import UIKit

@main
class AppDelegate: UIResponder, UIApplicationDelegate {
    /// Built once per process, before any scene connects, so launch reconciliation does not wait for a window.
    /// When the process hosts unit tests (the one place this is read) it never builds production services: they would
    /// run launch work against real storage in the test process and add to its process-wide result-buffer count. The
    /// scene still gets its window, showing the unavailable screen.
    let launcher: AppLauncher = {
        struct HostingUnitTests: Error {}
        guard ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil else {
            return AppLauncher(makeServices: { throw HostingUnitTests() })
        }
        #if DEBUG
        // A UI test selects a fixture with `-OhAndUITestFixture <name>`; a release build never reads the argument.
        if let fixtureName = UITestFixture.requestedName() {
            return AppLauncher(makeServices: { try UITestFixtureServices.make(named: fixtureName) })
        }
        #endif
        return AppLauncher()
    }()

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        launcher.launch()
        return true
    }

    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(
            name: "Default Configuration",
            sessionRole: connectingSceneSession.role
        )
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}
