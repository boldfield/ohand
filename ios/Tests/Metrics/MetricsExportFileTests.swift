import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The metrics file the Export screen hands to the share sheet: the service's JSON byte for byte, naming the build and
/// holding nothing but aggregates and machine labels.
final class MetricsExportFileTests: XCTestCase {
    private final class FixedReportCore: MetricsCore, @unchecked Sendable {
        let report: CoreMetricsReport
        private let lock = NSLock()
        private var builds: [CoreMetricsBuild?] = []
        var exportBuilds: [CoreMetricsBuild?] {
            lock.lock()
            defer { lock.unlock() }
            return builds
        }
        init(report: CoreMetricsReport) { self.report = report }
        func submit(
            _ request: CoreMetricsRequest,
            completion: @escaping @Sendable (Result<CoreMetricsAnswer, CoreFailure>) -> Void
        ) {
            if case .export(_, let build) = request {
                lock.lock()
                builds.append(build)
                lock.unlock()
            }
            completion(.success(CoreMetricsAnswer(outcome: "report", report: report)))
        }
    }

    private static let build = CoreMetricsBuild(
        version: "1.0", buildNumber: "42", revision: String(repeating: "a", count: 40), dirty: false)

    private static let fixtureReport = CoreMetricsReport(
        schema: "ohand.metrics.v1", generatedAt: "2026-11-07T10:00:00Z", recordingEnabled: true,
        sampleLimitPerSeries: 200, build: build,
        timings: [
            CoreMetricsTimingSummary(
                series: "trigger_to_ready", dimension: "cold", retainedSamples: 3, minMilliseconds: 300,
                medianMilliseconds: 400, p95Milliseconds: 500, maxMilliseconds: 500)
        ],
        failures: [CoreMetricsFailureSummary(series: "processing_failure", dimension: "timeout", occurrences: 2)])

    private var world: ExportWorld!

    override func setUpWithError() throws {
        try super.setUpWithError()
        world = try ExportWorld()
    }

    override func tearDown() {
        world.tearDown()
        super.tearDown()
    }

    private func makeControl(core: MetricsCore, build: CoreMetricsBuild?) -> (MetricsService, MetricsReportControl) {
        let service = MetricsService(core: core, session: world.session, build: build)
        return (service, MetricsReportControl(service: service, session: world.session, layout: world.layout))
    }

    func testTheFileIsTheServicesJsonByteForByteAndNamesTheBuild() async throws {
        world.authenticate()
        let (service, control) = makeControl(core: FixedReportCore(report: Self.fixtureReport), build: Self.build)

        let prepared = try await control.prepareExport()
        defer { control.cancelExport(exportID: prepared.id) }

        XCTAssertEqual(prepared.file.lastPathComponent, "MetricsExport.json")
        let expected = try await service.exportReport().json
        XCTAssertEqual(try Data(contentsOf: prepared.file), expected)
        let decoded = try JSONDecoder().decode(CoreMetricsReport.self, from: Data(contentsOf: prepared.file))
        XCTAssertEqual(decoded.build, Self.build)
    }

    func testTheProductionServiceAsksTheCoreToNameTheCurrentBuild() async throws {
        world.authenticate()
        let core = FixedReportCore(report: Self.fixtureReport)
        let service = MetricsService(core: core, session: world.session)
        let control = MetricsReportControl(service: service, session: world.session, layout: world.layout)

        let prepared = try await control.prepareExport()
        defer { control.cancelExport(exportID: prepared.id) }

        XCTAssertEqual(core.exportBuilds.count, 1)
        XCTAssertEqual(core.exportBuilds.first ?? nil, CoreMetricsBuild.current())
    }

    func testACaptureOnlySessionGetsNoFileAndNothingIsRequested() async throws {
        let (_, control) = makeControl(core: FixedReportCore(report: Self.fixtureReport), build: Self.build)

        do {
            _ = try await control.prepareExport()
            XCTFail("a capture-only session must not export")
        } catch {
            XCTAssertEqual(error as? MetricsReportFailure, .notAuthenticated)
        }
        let leftovers = try? FileManager.default.contentsOfDirectory(
            at: world.layout.directory(for: .temporaryFiles).appendingPathComponent("MetricsExports"),
            includingPropertiesForKeys: nil)
        XCTAssertTrue((leftovers ?? []).isEmpty)
    }

    func testRelockingRemovesThePreparedFile() async throws {
        world.authenticate()
        let (_, control) = makeControl(core: FixedReportCore(report: Self.fixtureReport), build: Self.build)
        let prepared = try await control.prepareExport()
        XCTAssertTrue(FileManager.default.fileExists(atPath: prepared.file.path))

        world.session.relock(.explicit)

        XCTAssertFalse(FileManager.default.fileExists(atPath: prepared.file.path))
        XCTAssertEqual(control.finishShare(exportID: prepared.id, ending: .completed).result, .interrupted)
    }
}
