import SwiftUI

/// What the app shows once its services exist: the composer home first, with search and settings in its glass header.
/// There is no backlog count, to-do list or catch-up flow on this screen.
struct AppRootView: View {
    static let noticeIdentifier = "app.management-notice"

    @ObservedObject var navigation: ManagementNavigationModel
    let captureEntry: CaptureEntryModel
    let context: ManagementContext
    /// The recent captures and lock states shown as the home body; nil leaves the body empty.
    var homeRecents: HomeRecentsModel?
    var destinations = ManagementCatalog.destinations
    var presentationReporter: ((ManagementPresentationEvent) -> Void)?
    var placeholderFrameReporter: (([String: CGRect]) -> Void)?
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        NavigationStack(path: $navigation.path) {
            CaptureEntryView(
                model: captureEntry,
                actionsAreDisabled: navigation.isAuthenticating,
                open: { navigation.open(entryID: $0) },
                notice: navigation.notice,
                homeBody: {
                    homeRecents.map { AnyView(HomeBodyView(model: $0)) } ?? AnyView(Color.clear)
                })
                .toolbar(.hidden, for: .navigationBar)
                .navigationDestination(for: String.self) { pathComponent in
                    destinationView(for: ManagementRoute(pathComponent: pathComponent))
                }
        }
        .onChange(of: scenePhase) { phase in
            switch phase {
            case .background: navigation.returnToCapture()
            case .active: navigation.revalidate()
            default: break
            }
        }
    }

    private func destinationView(for route: ManagementRoute) -> some View {
        destinations.view(for: route, context: context)
            .onAppear { presentationReporter?(.appeared(route, isPlaceholder: !destinations.isRegistered(route))) }
            .onDisappear { presentationReporter?(.disappeared(route)) }
            .onPreferenceChange(ManagementPlaceholderFrameKey.self) { placeholderFrameReporter?($0) }
    }
}

/// Shown when the services could not be built (for example, the storage directory cannot be created). Nothing can be
/// captured without them, so it says so and offers a retry instead of showing a capture surface that cannot save.
struct AppUnavailableView: View {
    static let retryIdentifier = "app.unavailable.retry"

    let retry: () -> Void

    var body: some View {
        VStack(spacing: 16) {
            Text("Oh And cannot save captures right now.")
                .font(.headline)
                .multilineTextAlignment(.center)
            Text("Nothing was lost. If the device was just restarted, unlock it once and try again.")
                .font(.body)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
            Button(action: retry) {
                Text("Try again").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .accessibilityIdentifier(Self.retryIdentifier)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding()
    }
}
