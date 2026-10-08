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

    // Runs in the foreground app process, possibly before or after the scene enters the foreground. It registers
    // the handoff, which either creates the pending entry the scene will commit or claims the entry the scene
    // already committed, so one activation yields one capture ID in both orders.
    func perform() async throws -> some IntentResult {
        IngressFlow.live.registerControlHandoffAndAnnounce()
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
