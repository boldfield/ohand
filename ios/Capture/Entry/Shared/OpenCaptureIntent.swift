import AppIntents

// Compiled into both OhAndApp and OhAndCaptureControl. A control that launches its host app must use an
// OpenIntent that is a member of both targets; the system then runs it in the foreground app.
struct OpenCaptureIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Oh And Capture"

    @Parameter(title: "Destination", default: .capture)
    var target: CaptureDestination

    init() {}

    init(target: CaptureDestination) {
        self.target = target
    }

    // Runs in the foreground app process, before or after the scene reaches the foreground. It only registers a
    // request for a native capture surface; the scene presents it. Nothing is persisted or awaited here.
    func perform() async throws -> some IntentResult {
        CaptureEntryHandoff.live.registerAndAnnounce(source: .controlIntent, requestedMode: target.requestedMode)
        return .result()
    }
}

enum CaptureDestination: String, AppEnum {
    /// The user's configured default capture mode.
    case capture
    case voice
    case silent

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Oh And Screen"
    static let caseDisplayRepresentations: [CaptureDestination: DisplayRepresentation] = [
        .capture: "Capture",
        .voice: "Voice capture",
        .silent: "Silent capture",
    ]

    var requestedMode: CaptureEntryMode? {
        switch self {
        case .capture: return nil
        case .voice: return .voice
        case .silent: return .silent
        }
    }
}
