import AppIntents
import Foundation

// Compiled into both CaptureProbe and CaptureProbeControl. A control that launches its host app must use an
// OpenIntent that is a member of both targets; the system then runs it in the foreground app.
struct ProbeOpenCaptureIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Capture Probe"

    @Parameter(title: "Destination", default: .capture)
    var target: ProbeCaptureDestination

    init() {}

    init(target: ProbeCaptureDestination) {
        self.target = target
    }

    // Runs in the foreground app process. It only registers the pending entry (one durable capture ID); the
    // scene commits it when it presents, so the ID is the same whichever of the two happens first.
    func perform() async throws -> some IntentResult {
        let captureId = IngressFlow.live.registerHandoff(source: .controlIntent)
        NotificationCenter.default.post(
            name: IngressFlow.handoffRegisteredNotification,
            object: nil,
            userInfo: ["captureId": captureId]
        )
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
