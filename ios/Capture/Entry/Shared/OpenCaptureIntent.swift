import AppIntents

// Compiled into both OhAndApp and OhAndCaptureControl: an openAppWhenRun intent only opens the app when it is also a member of the host app target.
struct OpenCaptureIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Oh And Capture"
    static let openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        .result()
    }
}
