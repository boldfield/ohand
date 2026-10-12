import Foundation

enum CaptureLaunchKind: String, Equatable {
    case cold
    case warm
}

/// What the device allows right now, as the app reports it. Entry itself needs neither: it only writes.
struct CaptureEntryAccess: Equatable {
    /// False before the first unlock after a restart, when protected files cannot be written yet. A save then fails
    /// honestly and keeps the input; nothing pretends otherwise.
    var protectedDataAvailable: Bool
    /// True only while the read session is authenticated. Reading any stored item (including its status) requires
    /// it, so a capture made from a locked or capture-only session shows no stored content and reads nothing.
    var readAuthorized: Bool

    var canReadStoredStatus: Bool { protectedDataAvailable && readAuthorized }
}

/// What a foreground scene shows for one entry. It never carries earlier captures.
struct CaptureEntryPresentation: Equatable {
    let entryID: String
    let source: CaptureEntrySource
    let mode: CaptureEntryMode
    let launchKind: CaptureLaunchKind
    let access: CaptureEntryAccess
}

/// The shortcut entry: `ohand-capture://capture`, optionally `?mode=voice` or `?mode=silent`. The URL carries no
/// capture data and grants nothing beyond opening the capture surface; an unknown mode falls back to the default.
enum CaptureEntryURL {
    static let scheme = "ohand-capture"
    static let host = "capture"

    /// `nil` when `url` is not a capture entry URL; otherwise the explicitly requested mode, if any.
    static func parse(_ url: URL) -> (isEntry: Bool, requestedMode: CaptureEntryMode?) {
        guard url.scheme?.lowercased() == scheme, url.host?.lowercased() == host else { return (false, nil) }
        let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        let modeValue = components?.queryItems?.first(where: { $0.name == "mode" })?.value
        return (true, modeValue.flatMap { CaptureEntryMode(rawValue: $0.lowercased()) })
    }
}

/// UIKit-free per-scene coordination: foreground state, cold or warm classification, and presenting a pending entry
/// request immediately (scene in the foreground) or on the next foreground entry.
///
/// A plain launch presents nothing. The capture surface appears only for a request, and a request is presented once,
/// so the order and latency of the intent's `perform()` against the scene callbacks cannot create a second entry.
/// Presenting never waits for a network, a provider, the management shell or a webview.
final class CaptureEntrySession {
    private let handoff: CaptureEntryHandoff
    private let defaultMode: () -> CaptureEntryMode
    private let access: () -> CaptureEntryAccess
    private let notificationCenter: NotificationCenter
    private var hasEnteredForeground = false
    private var observer: NSObjectProtocol?
    private(set) var isForeground = false
    private(set) var launchKind: CaptureLaunchKind = .cold

    init(
        handoff: CaptureEntryHandoff,
        defaultMode: @escaping () -> CaptureEntryMode,
        access: @escaping () -> CaptureEntryAccess,
        notificationCenter: NotificationCenter = .default
    ) {
        self.notificationCenter = notificationCenter
        self.handoff = handoff
        self.defaultMode = defaultMode
        self.access = access
    }

    deinit {
        if let observer = observer {
            notificationCenter.removeObserver(observer)
        }
    }

    /// Fires for the cold launch and for every later return from the background (warm launch).
    func willEnterForeground() -> CaptureEntryPresentation? {
        isForeground = true
        launchKind = hasEnteredForeground ? .warm : .cold
        hasEnteredForeground = true
        return presentPending()
    }

    func didEnterBackground() {
        isForeground = false
    }

    /// Handles a URL the scene received. A request that arrives while the scene is not foreground stays pending and is
    /// presented by the next `willEnterForeground`.
    func receive(url: URL) -> CaptureEntryPresentation? {
        let parsed = CaptureEntryURL.parse(url)
        guard parsed.isEntry else { return nil }
        handoff.register(source: .shortcutURL, requestedMode: parsed.requestedMode)
        return isForeground ? presentPending() : nil
    }

    /// Calls `onPresentation` for requests the intent announces. Safe to call once per scene.
    func observeHandoffs(onPresentation: @escaping (CaptureEntryPresentation) -> Void) {
        if let observer = observer {
            notificationCenter.removeObserver(observer)
        }
        observer = notificationCenter.addObserver(
            forName: CaptureEntryHandoff.registeredNotification, object: handoff, queue: .main
        ) { [weak self] _ in
            guard let self = self, self.isForeground, let presentation = self.presentPending() else { return }
            onPresentation(presentation)
        }
    }

    private func presentPending() -> CaptureEntryPresentation? {
        guard let request = handoff.takePending() else { return nil }
        return CaptureEntryPresentation(
            entryID: request.entryID,
            source: request.source,
            mode: request.requestedMode ?? defaultMode(),
            launchKind: launchKind,
            access: access())
    }
}
