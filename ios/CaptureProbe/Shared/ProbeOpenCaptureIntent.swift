import AppIntents

// Compiled into both CaptureProbe and CaptureProbeControl: an openAppWhenRun intent only opens the app when it is also a member of the host app target.
struct ProbeOpenCaptureIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Capture Probe"
    static let openAppWhenRun = true

    func perform() async throws -> some IntentResult {
        .result()
    }
}
