import Foundation

// Compiled into both OhAndApp and OhAndCaptureControl, next to the OpenIntent that calls it. Foundation only: the
// control extension target cannot use UIApplication, so anything about device state is passed in by the app.

/// How the user wants to capture. Both are native surfaces; neither involves the management shell or a webview.
enum CaptureEntryMode: String, Equatable {
    case voice
    case silent
}

/// The system entry that asked for a capture surface. A plain app launch is not an entry.
enum CaptureEntrySource: String, Equatable {
    case controlIntent
    case shortcutURL
}

/// One request to show the capture surface. It carries no user input: the capture itself (and its capture ID) only
/// comes into being when the user records or saves, so a request that is lost costs one more tap, never a capture.
struct CaptureEntryRequest: Equatable {
    let entryID: String
    let source: CaptureEntrySource
    /// `nil` means the user's configured default mode.
    let requestedMode: CaptureEntryMode?
}

/// Holds the one pending entry request between the moment the system runs the entry (the intent's `perform()`, which
/// may run before or after the scene reaches the foreground) and the moment a foreground scene presents it.
///
/// * A request is minted once. A second handoff before the first was presented (the system running `perform()` twice,
///   a double tap) adopts the pending request: same ID, newest explicit mode. Only a request that was taken can be
///   followed by a new one, so one press never yields two presentations.
/// * Nothing is persisted: a request holds no user input. Nothing here waits for any other component.
final class CaptureEntryHandoff {
    static let registeredNotification = Notification.Name("com.boldfield.ohand.capture.entry.registered")
    static let live = CaptureEntryHandoff()

    private let notificationCenter: NotificationCenter
    private let makeEntryID: () -> String
    private let lock = NSLock()
    private var pending: CaptureEntryRequest?

    init(
        notificationCenter: NotificationCenter = .default,
        makeEntryID: @escaping () -> String = { UUID().uuidString }
    ) {
        self.notificationCenter = notificationCenter
        self.makeEntryID = makeEntryID
    }

    /// Registers a request and returns it. Adopts a request that is still pending.
    @discardableResult
    func register(source: CaptureEntrySource, requestedMode: CaptureEntryMode?) -> CaptureEntryRequest {
        lock.lock()
        defer { lock.unlock() }
        if let existing = pending {
            let adopted = CaptureEntryRequest(
                entryID: existing.entryID, source: existing.source,
                requestedMode: requestedMode ?? existing.requestedMode)
            pending = adopted
            return adopted
        }
        let created = CaptureEntryRequest(entryID: makeEntryID(), source: source, requestedMode: requestedMode)
        pending = created
        return created
    }

    /// Registers a request, then tells a foreground scene that one is pending.
    @discardableResult
    func registerAndAnnounce(source: CaptureEntrySource, requestedMode: CaptureEntryMode?) -> CaptureEntryRequest {
        let request = register(source: source, requestedMode: requestedMode)
        notificationCenter.post(
            name: Self.registeredNotification, object: self, userInfo: ["entryID": request.entryID])
        return request
    }

    /// Removes and returns the pending request. A request is presented at most once.
    func takePending() -> CaptureEntryRequest? {
        lock.lock()
        defer { lock.unlock() }
        let request = pending
        pending = nil
        return request
    }

    var hasPending: Bool {
        lock.lock()
        defer { lock.unlock() }
        return pending != nil
    }
}
