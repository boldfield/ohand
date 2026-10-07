import AppIntents
import SwiftUI
import WidgetKit

@available(iOS 18.0, *)
struct ProbeCaptureControl: ControlWidget {
    static let kind = "com.boldfield.ohand.probes.capture.control.entry"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: ProbeOpenCaptureIntent()) {
                Label("Capture Probe", systemImage: "mic")
            }
        }
        .displayName("Capture Probe")
    }
}

@available(iOS 18.0, *)
struct ProbeOpenCaptureIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Capture Probe"
    static let openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        .result()
    }
}

@main
struct ProbeControlBundle: WidgetBundle {
    var body: some Widget {
        if #available(iOS 18.0, *) {
            ProbeCaptureControl()
        }
    }
}
