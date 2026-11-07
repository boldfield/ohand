import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

@MainActor
final class MetricsExportSettingsModelTests: XCTestCase {
    private final class FakeMetricsControls: MetricsReportControlling {
        var enabled: Bool? = true
        var setCalls: [Bool] = []
        var clearCalls = 0
        var prepareFailure: MetricsReportFailure?
        var prepared: [String] = []
        var finished: [(String, ExportShareEnding)] = []
        var cancelled: [String] = []
        var shareResult: ExportShareResult = .shared

        func recordingEnabled() async -> Bool? { enabled }
        func setRecordingEnabled(_ enabled: Bool) async -> Bool {
            setCalls.append(enabled)
            self.enabled = enabled
            return true
        }
        func clearAll() async -> Bool {
            clearCalls += 1
            return true
        }
        func prepareExport() async throws -> PreparedMetricsExport {
            if let prepareFailure { throw prepareFailure }
            let id = "metrics-\(prepared.count + 1)"
            prepared.append(id)
            return PreparedMetricsExport(id: id, file: URL(fileURLWithPath: "/tmp/\(id)/MetricsExport.json"))
        }
        func finishShare(exportID: String, ending: ExportShareEnding) -> ExportShareOutcome {
            finished.append((exportID, ending))
            return ExportShareOutcome(result: shareResult, temporariesRemoved: true)
        }
        func cancelExport(exportID: String) -> ExportShareOutcome {
            cancelled.append(exportID)
            return ExportShareOutcome(result: .cancelled, temporariesRemoved: true)
        }
    }

    private var scope = CompositionReadScope.authenticated

    private func makeModel(_ controls: FakeMetricsControls) -> MetricsExportSettingsModel {
        MetricsExportSettingsModel(controls: controls, readScope: { [unowned self] in scope })
    }

    func testRecordingIsOnByDefaultAndTheSectionIsAvailableInAnAuthenticatedSession() async {
        let model = makeModel(FakeMetricsControls())
        await model.refresh()
        XCTAssertTrue(model.isRecording)
        XCTAssertTrue(model.isAvailable)
        XCTAssertEqual(model.phase, .idle)
    }

    func testACaptureOnlySessionIsToldWhyAndNothingIsRequested() async {
        scope = .captureOnly
        let controls = FakeMetricsControls()
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertFalse(model.isAvailable)
        await model.startExport()
        await model.setRecording(false)
        await model.clearMetrics()
        XCTAssertTrue(controls.prepared.isEmpty)
        XCTAssertTrue(controls.setCalls.isEmpty)
        XCTAssertEqual(controls.clearCalls, 0)
        XCTAssertFalse(ExportSettingsText.metricsLockedText.isEmpty)
    }

    func testExportPresentsTheFileAndReportsSharedOnlyFromTheServicesOutcome() async {
        let controls = FakeMetricsControls()
        let model = makeModel(controls)
        await model.startExport()
        XCTAssertEqual(model.phase, .sharing)
        XCTAssertEqual(model.pendingShare?.id, "metrics-1")

        model.shareEnded(.completed, exportID: "metrics-1")

        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(model.phase, .finished(ExportSettingsText.metricsShared(temporariesRemoved: true)))
    }

    func testACancelledShareIsNotShownAsShared() async {
        let controls = FakeMetricsControls()
        controls.shareResult = .cancelled
        let model = makeModel(controls)
        await model.startExport()
        model.shareEnded(.cancelled, exportID: "metrics-1")
        XCTAssertEqual(model.phase, .finished(ExportSettingsText.cancelled(temporariesRemoved: true)))
    }

    func testAnEndingForAnotherExportIsIgnored() async {
        let controls = FakeMetricsControls()
        let model = makeModel(controls)
        await model.startExport()
        model.shareEnded(.completed, exportID: "metrics-other")
        XCTAssertEqual(model.phase, .sharing)
        XCTAssertTrue(controls.finished.isEmpty)
    }

    func testLeavingTheScreenCancelsAPendingFile() async {
        let controls = FakeMetricsControls()
        let model = makeModel(controls)
        await model.startExport()
        model.clear()
        XCTAssertEqual(controls.cancelled, ["metrics-1"])
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(model.phase, .idle)
    }

    func testAnExportFailureSaysSoAndARelockLocksTheSection() async {
        let controls = FakeMetricsControls()
        controls.prepareFailure = .unavailable
        let model = makeModel(controls)
        await model.startExport()
        XCTAssertEqual(model.phase, .finished(ExportSettingsText.metricsExportFailed))

        controls.prepareFailure = .notAuthenticated
        await model.startExport()
        XCTAssertEqual(model.phase, .locked)
    }

    func testPauseAndClearUseTheServiceAndSayWhatHappened() async {
        let controls = FakeMetricsControls()
        let model = makeModel(controls)
        await model.setRecording(false)
        XCTAssertEqual(controls.setCalls, [false])
        XCTAssertFalse(model.isRecording)
        XCTAssertEqual(model.phase, .finished(ExportSettingsText.metricsPaused))

        await model.setRecording(true)
        XCTAssertTrue(model.isRecording)

        await model.clearMetrics()
        XCTAssertEqual(controls.clearCalls, 1)
        XCTAssertEqual(model.phase, .finished(ExportSettingsText.metricsCleared))
    }

    func testTheCopySaysWhatTheFileHoldsAndDoesNotHold() {
        XCTAssertTrue(ExportSettingsText.metricsHolds.contains("timings and counts"))
        XCTAssertTrue(ExportSettingsText.metricsHoldsNot.contains("no note content"))
    }
}
