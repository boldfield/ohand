import AppIntents
import SwiftUI
import WidgetKit

@available(iOS 18.0, *)
struct OhAndCaptureControl: ControlWidget {
    static let kind = "com.boldfield.ohand.app.capture-control.entry"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: OpenCaptureIntent()) {
                Label("Capture", systemImage: "mic")
            }
        }
        .displayName("Oh And Capture")
    }
}

@available(iOS 18.0, *)
struct OpenCaptureIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Oh And Capture"
    static let openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        .result()
    }
}

@main
struct OhAndCaptureControlBundle: WidgetBundle {
    var body: some Widget {
        if #available(iOS 18.0, *) {
            OhAndCaptureControl()
        }
    }
}
