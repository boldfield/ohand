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

    func perform() async throws -> some IntentResult {
        .result()
    }
}

enum CaptureDestination: String, AppEnum {
    case capture

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Oh And Screen"
    static let caseDisplayRepresentations: [CaptureDestination: DisplayRepresentation] = [
        .capture: "Capture",
    ]
}
