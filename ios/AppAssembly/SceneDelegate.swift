import OhAndServices
import UIKit
import SwiftUI

class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var sceneCover: SceneCoverController?

    private var launcher: AppLauncher? {
        (UIApplication.shared.delegate as? AppDelegate)?.launcher
    }

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene), let launcher = launcher else { return }
        launcher.launch()
        sceneCover = SceneCoverController(presenter: WindowRedactionPresenter(windowScenes: { [weak windowScene] in
            windowScene.map { [$0] } ?? []
        }))

        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = UIHostingController(rootView: AppLauncherView(launcher: launcher))
        self.window = window
        window.makeKeyAndVisible()

        for context in connectionOptions.urlContexts {
            launcher.services?.receive(url: context.url)
        }
        if !connectionOptions.urlContexts.isEmpty { sceneCover?.captureEntryArrived() }
    }

    func sceneWillResignActive(_ scene: UIScene) {
        sceneCover?.sceneWillResignActive()
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        sceneCover?.sceneDidBecomeActive()
    }

    func sceneWillEnterForeground(_ scene: UIScene) {
        launcher?.sceneWillEnterForeground()
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        launcher?.services?.sceneDidEnterBackground()
    }

    func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
        for context in URLContexts {
            launcher?.services?.receive(url: context.url)
        }
        sceneCover?.captureEntryArrived()
    }
}
