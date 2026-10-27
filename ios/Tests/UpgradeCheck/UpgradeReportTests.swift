#if DEBUG
import XCTest
import OhAndCoreBridge
@testable import OhAnd

final class UpgradeReportTests: XCTestCase {
    private func sampleReport() -> UpgradeReport {
        UpgradeReport(
            schema: UpgradeReport.schema, generatedAt: "2026-10-27T10:00:00Z",
            build: CoreMetricsBuild(
                version: "0.1.0", buildNumber: "12", revision: String(repeating: "ab", count: 20), dirty: false),
            recentItemCount: 1,
            items: [
                UpgradeReport.ItemEntry(
                    itemID: "item-1", revision: 2, itemType: "note", lifecycleState: "active", textBasis: "corrected",
                    textLength: 18, originalTextLength: 21, correctedTextLength: 18,
                    reminder: UpgradeReport.ReminderEntry(
                        reminderID: "reminder-1", requestState: "resolved", scheduleState: "scheduled",
                        scheduleGeneration: 1, resolvedInstant: "2026-10-27T11:00:00Z"))
            ],
            probes: [UpgradeReport.ProbeEntry(itemID: "item-2", state: "not_found")],
            providers: [
                UpgradeReport.ProviderEntry(
                    profileVersion: "v1", providerProtocol: "open_ai", model: "synthetic-model", destinationClasses: ["cloud"],
                    hasEndpoint: false, credentialBound: true,
                    credentialStatus: "present", revoked: false, defaultFor: ["text_interpretation"], routeCovered: true)
            ],
            pendingReminderRequests: [UpgradeReport.PendingReminderRequest(reminderID: "reminder-1", scheduleGeneration: 1)],
            otherPendingRequestCount: 0)
    }

    func testTheReportRoundTripsAndNamesTheBuild() throws {
        let report = sampleReport()
        let decoded = try JSONDecoder().decode(UpgradeReport.self, from: try report.encoded())
        XCTAssertEqual(decoded, report)
        XCTAssertEqual(decoded.build?.buildNumber, "12")
        XCTAssertEqual(decoded.schema, "ohand.upgrade-report.v1")
    }

    func testTheReportHoldsOnlyCountsStatesIdentifiersAndInstants() throws {
        let object = try XCTUnwrap(try JSONSerialization.jsonObject(with: try sampleReport().encoded()) as? [String: Any])
        var keys = Set<String>()
        func collect(_ value: Any) {
            if let dictionary = value as? [String: Any] {
                for (key, nested) in dictionary {
                    keys.insert(key)
                    collect(nested)
                }
            } else if let array = value as? [Any] {
                array.forEach(collect)
            }
        }
        collect(object)
        // Every field is a length, a count, a state label, an identifier, a flag or an instant; none can carry text.
        XCTAssertEqual(
            keys,
            [
                "schema", "generated_at", "build", "version", "build_number", "revision", "dirty", "recent_item_count",
                "items", "item_id", "revision", "item_type", "lifecycle_state", "text_basis", "text_length",
                "original_text_length", "corrected_text_length", "reminder", "reminder_id", "request_state",
                "schedule_state", "schedule_generation", "resolved_instant", "probes", "state", "providers",
                "profile_version", "protocol", "model", "destination_classes", "has_endpoint", "credential_bound", "credential_status", "revoked",
                "default_for", "route_covered", "pending_reminder_requests", "other_pending_request_count",
            ])
    }

    func testTheRequestComesFromTheLaunchArgumentsOnly() {
        XCTAssertNil(UpgradeReportRequest.requested(arguments: ["OhAnd"]))
        XCTAssertEqual(UpgradeReportRequest.requested(arguments: ["OhAnd", "-OhAndUpgradeReport"])?.probeItemIDs, [])
        XCTAssertEqual(
            UpgradeReportRequest.requested(arguments: ["OhAnd", "-OhAndUpgradeReport", "-OhAndUpgradeProbe", "a,b,,c"])?
                .probeItemIDs, ["a", "b", "c"])
    }
}
#endif
