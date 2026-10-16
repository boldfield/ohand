import UIKit
import SwiftUI

class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

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
    }
}
