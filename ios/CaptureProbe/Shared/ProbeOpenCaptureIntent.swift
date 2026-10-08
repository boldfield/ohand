import AppIntents

/// ProbeOpenCaptureIntent must be a member of both the CaptureProbe app target and the
/// ProbeControlBundle (Shortcuts) targets so the control can activate it and the app can
/// receive it through the scene delegate's connectionOptions.
struct ProbeOpenCaptureIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Capture Probe"

    @Parameter(title: "Destination", default: .capture)
    var target: ProbeCaptureDestination

    init() {}

    init(target: ProbeCaptureDestination) {
        self.target = target
    }

    func perform() async throws -> some IntentResult {
        // Generate a unique ID for this control activation and pass it as a user activity
        // so the app can tie this handoff to a single consistent capture record.
        let captureId = UUID().uuidString
        let activity = NSUserActivity(activityType: "com.boldfield.ohand.probes.capture.intent")
        activity.userInfo = ["captureId": captureId]
        NSUserActivity.current = activity
        return .result()
    }
}

enum ProbeCaptureDestination: String, AppEnum {
    case capture

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Capture Probe Screen"
    static let caseDisplayRepresentations: [ProbeCaptureDestination: DisplayRepresentation] = [
        .capture: "Capture",
    ]
}
