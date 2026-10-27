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
            let fixtureLaunch = UITestFixtureLaunch(name: fixtureName)
            return AppLauncher(
                makeServices: { try fixtureLaunch.makeServices() },
                startServices: { services, isActive in
                    services.start(isActive: isActive)
                    fixtureLaunch.seedInBackground()
                })
        }
        // The upgrade check asks a debug build for a content-free report of its own data once the app's own authentication
        // succeeds (`-OhAndUpgradeReport`); a release build never reads the argument.
        if let upgradeReport = UpgradeReportRequest.requested() {
            return AppLauncher(startServices: { services, isActive in
                services.start(isActive: isActive)
                let writer = UpgradeReportWriter(root: services.root, request: upgradeReport)
                writer.begin()
                AppDelegate.upgradeReportWriter = writer
            })
        }
        #endif
        return AppLauncher()
    }()

    #if DEBUG
    /// Keeps the debug upgrade report writer alive for the process.
    static var upgradeReportWriter: UpgradeReportWriter?
    #endif

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
