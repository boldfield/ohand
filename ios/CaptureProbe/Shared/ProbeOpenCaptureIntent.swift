import AppIntents

struct ProbeOpenCaptureIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Capture Probe"

    @Parameter(title: "Destination", default: .capture)
    var target: ProbeCaptureDestination

    init() {}

    init(target: ProbeCaptureDestination) {
        self.target = target
    }

    func perform() async throws -> some IntentResult {
        .result()
    }
}

enum ProbeCaptureDestination: String, AppEnum {
    case capture

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Capture Probe Screen"
    static let caseDisplayRepresentations: [ProbeCaptureDestination: DisplayRepresentation] = [
        .capture: "Capture",
    ]
}
