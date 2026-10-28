import AppIntents
import SwiftUI
import WidgetKit

@available(iOS 18.0, *)
struct OhAndCaptureControl: ControlWidget {
    static let kind = "com.boldfield.ohand.app.capture-control.entry"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: OpenCaptureIntent(target: .voice)) {
                Label("Oh And Voice", systemImage: "mic")
            }
        }
        .displayName("Oh And Voice")
    }
}

@available(iOS 18.0, *)
struct OhAndNoteControl: ControlWidget {
    static let kind = "com.boldfield.ohand.app.capture-control.note"

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: Self.kind) {
            ControlWidgetButton(action: OpenCaptureIntent(target: .silent)) {
                Label("Oh And Note", systemImage: "square.and.pencil")
            }
        }
        .displayName("Oh And Note")
    }
}

@main
struct OhAndCaptureControlBundle: WidgetBundle {
    var body: some Widget {
        if #available(iOS 18.0, *) {
            OhAndCaptureControl()
            OhAndNoteControl()
        }
    }
}
