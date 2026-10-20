import SwiftUI

/// The scene's root: the capture-first app once the services exist, otherwise the retry screen.
struct AppLauncherView: View {
    @ObservedObject var launcher: AppLauncher

    var body: some View {
        if let services = launcher.services {
            AppRootView(
                navigation: services.navigation, captureEntry: services.captureEntry, context: services.managementContext,
                homeRecents: services.homeRecents)
        } else {
            AppUnavailableView(retry: { launcher.launch() })
        }
    }
}
