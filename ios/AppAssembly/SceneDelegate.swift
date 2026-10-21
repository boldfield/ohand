import OhAndServices
import UIKit
import SwiftUI

class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var sceneCover: SceneCoverController?

    /// Built once the services exist, which can be after the scene connects when the first build failed.
    private func cover() -> SceneCoverController? {
        if sceneCover == nil, let root = launcher?.services?.root, let windowScene = window?.windowScene {
            sceneCover = SceneCoverController(
                presenter: WindowRedactionPresenter(windowScenes: { [weak windowScene] in
                    windowScene.map { [$0] } ?? []
                }),
                readLock: root)
        }
        return sceneCover
    }

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

        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = UIHostingController(rootView: AppLauncherView(launcher: launcher))
        self.window = window
        window.makeKeyAndVisible()

        for context in connectionOptions.urlContexts {
            launcher.services?.receive(url: context.url)
        }
    }

    func sceneWillResignActive(_ scene: UIScene) {
        cover()?.sceneWillResignActive()
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        cover()?.sceneDidBecomeActive()
    }

    func sceneWillEnterForeground(_ scene: UIScene) {
        launcher?.sceneWillEnterForeground()
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        cover()?.sceneDidEnterBackground()
        launcher?.services?.sceneDidEnterBackground()
    }

    func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
        for context in URLContexts {
            launcher?.services?.receive(url: context.url)
        }
    }
}
