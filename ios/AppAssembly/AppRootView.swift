import SwiftUI

/// What the app shows once its services exist: the capture surface first, with search and settings as secondary links
/// below it. There is no backlog count, to-do list or catch-up flow on this screen.
struct AppRootView: View {
    static let managementLinksIdentifier = "app.management-links"
    static let noticeIdentifier = "app.management-notice"

    @ObservedObject var navigation: ManagementNavigationModel
    let captureEntry: CaptureEntryModel
    let context: ManagementContext
    var destinations = ManagementCatalog.destinations
    var presentationReporter: ((ManagementPresentationEvent) -> Void)?
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        NavigationStack(path: $navigation.path) {
            CaptureEntryView(model: captureEntry)
                .navigationTitle("Oh And")
                .navigationBarTitleDisplayMode(.inline)
                .safeAreaInset(edge: .bottom) { managementLinks }
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
    }

    private var managementLinks: some View {
        VStack(spacing: 8) {
            if let notice = navigation.notice {
                Text(notice)
                    .font(.callout)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Self.noticeIdentifier)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { managementButtons }
                VStack(spacing: 8) { managementButtons }
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.managementLinksIdentifier)
    }

    private var managementButtons: some View {
        ForEach(ManagementCatalog.secondaryEntries) { entry in
            Button {
                navigation.open(entryID: entry.id)
            } label: {
                Label(entry.title, systemImage: entry.systemImage)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .disabled(navigation.isAuthenticating)
            .accessibilityIdentifier("management.open.\(entry.id)")
        }
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
