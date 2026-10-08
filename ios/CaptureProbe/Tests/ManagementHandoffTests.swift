import XCTest

final class ManagementHandoffTests: XCTestCase {
    private var rootDirectory: URL!

    override func setUpWithError() throws {
        rootDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ManagementHandoffTests-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: rootDirectory)
    }

    private func makeFlow() -> IngressFlow {
        IngressFlow(store: IngressStore(rootDirectory: rootDirectory))
    }

    // Simulator tests run on the host, so the shared fixture in the repository is readable at its source path.
    private func loadSharedVectors() throws -> [[String: Any]] {
        let repositoryRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let fixture = repositoryRoot.appendingPathComponent("probes/tauri-handoff/fixtures/handoff-urls.json")
        let data = try Data(contentsOf: fixture)
        let document = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        return try XCTUnwrap(document["cases"] as? [[String: Any]])
    }

    // MARK: URL grammar (same vectors as the Rust receiver)

    func testSharedVectorsAreAcceptedOrRejectedWithTheSameCodeAsTheRustReceiver() throws {
        let cases = try loadSharedVectors()
        XCTAssertGreaterThan(cases.count, 30, "vector file looks truncated")
        for vector in cases {
            let description = try XCTUnwrap(vector["description"] as? String)
            let url = try XCTUnwrap(vector["url"] as? String)
            let expectation = try XCTUnwrap(vector["expect"] as? String)
            let result = ManagementHandoff.captureId(fromHandoffURL: url)
            if expectation == "ok" {
                XCTAssertEqual(try? result.get(), vector["captureId"] as? String, description)
            } else if case .failure(let error) = result {
                XCTAssertEqual(error.rawValue, expectation, description)
            } else {
                XCTFail("\(description): expected rejection \(expectation), got acceptance")
            }
        }
    }

    func testBuiltURLCarriesExactlyTheCanonicalIdentifier() throws {
        let captureId = "0F8FAD5B-D9CB-469F-A165-70867728950E"
        let url = try XCTUnwrap(ManagementHandoff.url(forCaptureId: captureId))
        XCTAssertEqual(url.absoluteString, "ohand-tauri://capture?captureId=\(captureId)")
        XCTAssertEqual(try? ManagementHandoff.captureId(fromHandoffURL: url.absoluteString).get(), captureId)
    }

    func testNoURLIsBuiltForANonCanonicalIdentifier() {
        XCTAssertNil(ManagementHandoff.url(forCaptureId: "0f8fad5b-d9cb-469f-a165-70867728950e"))
        XCTAssertNil(ManagementHandoff.url(forCaptureId: "evil"))
        XCTAssertNil(ManagementHandoff.url(forCaptureId: ""))
        XCTAssertNil(ManagementHandoff.url(forCaptureId: "0F8FAD5B-D9CB-469F-A165-70867728950E&x=1"))
    }

    func testFoundationUUIDStringsAreCanonical() {
        for _ in 0..<50 {
            XCTAssertTrue(ManagementHandoff.isCanonicalCaptureId(UUID().uuidString))
        }
    }

    // MARK: Handoff from a committed entry

    func testColdAndWarmEntriesEachOfferAHandoffCarryingTheirOwnRecordIdentifier() throws {
        let flow = makeFlow()
        let session = IngressSession(flow: flow)

        flow.registerHandoff(source: .shortcutURL)
        let cold = try XCTUnwrap(session.willEnterForeground(protectedDataAvailable: true))
        XCTAssertEqual(cold.launchKind, .cold)
        let coldURL = try XCTUnwrap(cold.managementHandoffURL)
        XCTAssertEqual(try? ManagementHandoff.captureId(fromHandoffURL: coldURL.absoluteString).get(), cold.captureId)
        XCTAssertNotNil(flow.store.loadRecord(captureId: cold.captureId))

        session.didEnterBackground()
        flow.registerHandoff(source: .shortcutURL)
        let warm = try XCTUnwrap(session.willEnterForeground(protectedDataAvailable: true))
        XCTAssertEqual(warm.launchKind, .warm)
        XCTAssertNotEqual(warm.captureId, cold.captureId)
        let warmURL = try XCTUnwrap(warm.managementHandoffURL)
        XCTAssertEqual(try? ManagementHandoff.captureId(fromHandoffURL: warmURL.absoluteString).get(), warm.captureId)
        XCTAssertNotNil(flow.store.loadRecord(captureId: warm.captureId))
    }

    func testSavingNeverDependsOnTheManagementShell() throws {
        let flow = makeFlow()
        flow.registerHandoff(source: .controlIntent)
        let outcome = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))
        // The record is durable before any handoff URL has been built or opened.
        XCTAssertEqual(outcome.status, .saved)
        XCTAssertNotNil(flow.store.loadRecord(captureId: outcome.captureId))
        XCTAssertEqual(flow.store.allRecords().count, 1)
    }

    func testAReplayedEntryStillOffersItsHandoffAndAFailedOneOffersNone() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)
        let saved = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))
        let replayed = flow.commitHandoff(
            captureId: handoffId, source: .controlIntent, launchKind: .warm, protectedDataAvailable: true
        )
        XCTAssertEqual(replayed.status, .replayed)
        XCTAssertEqual(replayed.managementHandoffURL, saved.managementHandoffURL)

        let failed = IngressOutcome(
            captureId: handoffId,
            source: .controlIntent,
            launchKind: .cold,
            protectedDataAvailable: false,
            status: .failed("write error")
        )
        XCTAssertNil(failed.managementHandoffURL)
    }
}
