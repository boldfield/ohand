import AppIntents
import SwiftUI
import WidgetKit

@available(iOS 18.0, *)
struct ProbeCaptureControl: ControlWidget {
    static let kind = "com.boldfield.ohand.probes.capture.control.entry"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: ProbeOpenCaptureIntent(target: .capture)) {
                Label("Capture Probe", systemImage: "mic")
            }
        }
        .displayName("Capture Probe")
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
