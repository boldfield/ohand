#if DEBUG
import Foundation
import OhAndCoreBridge
import OhAndServices
import UserNotifications

/// What a debug build says about its own data after an upgrade: counts, states, identifiers the synthetic run created and
/// instants. Never text: an item is described by its identifier, state and the length of each text it holds. The report is
/// read by `tools/release/simulator_upgrade_check.sh`, which compares it with what the seeding run did and with the report
/// after a relaunch. This file and the rest of `UpgradeCheck/` are compiled out of a release build, so a release app has no
/// code that can write one.
struct UpgradeReport: Codable, Equatable {
    static let schema = "ohand.upgrade-report.v1"

    struct ItemEntry: Codable, Equatable {
        let itemID: String
        let revision: Int
        let itemType: String?
        let lifecycleState: String
        /// `original`, `transcribed` or `corrected`: which stored text is the authoritative one.
        let textBasis: String
        let textLength: Int
        let originalTextLength: Int?
        let correctedTextLength: Int?
        let reminder: ReminderEntry?

        enum CodingKeys: String, CodingKey {
            case itemID = "item_id"
            case revision
            case itemType = "item_type"
            case lifecycleState = "lifecycle_state"
            case textBasis = "text_basis"
            case textLength = "text_length"
            case originalTextLength = "original_text_length"
            case correctedTextLength = "corrected_text_length"
            case reminder
        }
    }

    struct ReminderEntry: Codable, Equatable {
        let reminderID: String
        let requestState: String
        let scheduleState: String
        let scheduleGeneration: Int
        let resolvedInstant: String?

        enum CodingKeys: String, CodingKey {
            case reminderID = "reminder_id"
            case requestState = "request_state"
            case scheduleState = "schedule_state"
            case scheduleGeneration = "schedule_generation"
            case resolvedInstant = "resolved_instant"
        }
    }

    /// An item the run asked about by identifier: `active`, or `not_found` for an item that is deleted or unknown (the
    /// services do not tell those apart).
    struct ProbeEntry: Codable, Equatable {
        let itemID: String
        let state: String

        enum CodingKeys: String, CodingKey {
            case itemID = "item_id"
            case state
        }
    }

    struct ProviderEntry: Codable, Equatable {
        let profileVersion: String
        let providerProtocol: String
        let hasEndpoint: Bool
        let credentialBound: Bool
        /// `present`, `absent` or `invalidated`; `unreadable` when the Keychain could not be asked.
        let credentialStatus: String
        let revoked: Bool
        let defaultFor: [String]
        /// True when a route grant covers this exact version, so processing can route to it.
        let routeCovered: Bool

        enum CodingKeys: String, CodingKey {
            case profileVersion = "profile_version"
            case providerProtocol = "protocol"
            case hasEndpoint = "has_endpoint"
            case credentialBound = "credential_bound"
            case credentialStatus = "credential_status"
            case revoked
            case defaultFor = "default_for"
            case routeCovered = "route_covered"
        }
    }

    /// A pending notification request whose identifier names a reminder (`<reminder id>#<generation>`).
    struct PendingReminderRequest: Codable, Equatable {
        let reminderID: String
        let scheduleGeneration: Int

        enum CodingKeys: String, CodingKey {
            case reminderID = "reminder_id"
            case scheduleGeneration = "schedule_generation"
        }
    }

    let schema: String
    let generatedAt: String
    let build: CoreMetricsBuild?
    let recentItemCount: Int
    let items: [ItemEntry]
    let probes: [ProbeEntry]
    let providers: [ProviderEntry]
    let pendingReminderRequests: [PendingReminderRequest]
    /// Pending requests whose identifier is not a reminder's (a daily prompt, for example).
    let otherPendingRequestCount: Int

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedAt = "generated_at"
        case build
        case recentItemCount = "recent_item_count"
        case items
        case probes
        case providers
        case pendingReminderRequests = "pending_reminder_requests"
        case otherPendingRequestCount = "other_pending_request_count"
    }

    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }
}

/// The launch arguments of the upgrade check: `-OhAndUpgradeReport` asks for a report once the app's own authentication
/// succeeds, and `-OhAndUpgradeProbe <id>[,<id>...]` names items to ask about by identifier.
struct UpgradeReportRequest: Equatable {
    static let reportArgument = "-OhAndUpgradeReport"
    static let probeArgument = "-OhAndUpgradeProbe"
    static let reportFileName = "upgrade-report.json"

    let probeItemIDs: [String]

    static func requested(arguments: [String] = ProcessInfo.processInfo.arguments) -> UpgradeReportRequest? {
        guard arguments.contains(reportArgument) else { return nil }
        var probes: [String] = []
        if let index = arguments.firstIndex(of: probeArgument), arguments.indices.contains(index + 1) {
            probes = arguments[index + 1].split(separator: ",").map(String.init).filter { !$0.isEmpty }
        }
        return UpgradeReportRequest(probeItemIDs: probes)
    }

    /// Where the report goes: the app's own data container, so the driving script reads it with `simctl get_app_container`.
    static func reportURL(fileManager: FileManager = .default) throws -> URL {
        let support = try fileManager.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        let directory = support.appendingPathComponent("OhAnd", isDirectory: true)
            .appendingPathComponent("UpgradeCheck", isDirectory: true)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appendingPathComponent(reportFileName)
    }
}

/// Reads the report from the running app's own services and writes it. It starts only when the request is present, and
/// reads nothing until the root's read scope is authenticated by the app's own Face ID path, so a locked app writes nothing.
final class UpgradeReportWriter: @unchecked Sendable {
    static let authenticationTimeoutSeconds: UInt64 = 180

    private let root: ServiceCompositionRoot
    private let request: UpgradeReportRequest
    private let build: CoreMetricsBuild?
    private let destination: () throws -> URL
    private var task: Task<Void, Never>?

    init(
        root: ServiceCompositionRoot, request: UpgradeReportRequest,
        build: CoreMetricsBuild? = .current(),
        destination: @escaping () throws -> URL = { try UpgradeReportRequest.reportURL() }
    ) {
        self.root = root
        self.request = request
        self.build = build
        self.destination = destination
    }

    func begin() {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self, await self.waitForAuthentication() else { return }
            await self.writeReport()
        }
    }

    private func waitForAuthentication() async -> Bool {
        for _ in 0..<(Self.authenticationTimeoutSeconds * 2) {
            if root.readScope == .authenticated { return true }
            try? await Task.sleep(nanoseconds: 500_000_000)
        }
        return false
    }

    /// Item reads can race the launch passes; a failed read is tried again for a while before the report gives up.
    private func writeReport() async {
        for _ in 0..<20 {
            if let report = try? await collect(), let data = try? report.encoded(), let url = try? destination() {
                try? data.write(to: url, options: .atomic)
                return
            }
            try? await Task.sleep(nanoseconds: 1_000_000_000)
        }
    }

    private func collect() async throws -> UpgradeReport {
        var recent: [CoreRecentItem] = []
        var cursor: String?
        repeat {
            let page = try await root.recentItems.page(limit: RecentItemsService.maximumLimit, cursor: cursor)
            recent += page.items
            cursor = page.nextCursor
        } while cursor != nil
        var items: [UpgradeReport.ItemEntry] = []
        for entry in recent {
            let detail = try await root.itemDetails.detail(itemID: entry.itemID)
            let reminder = try await root.reminderControls.reminder(itemID: entry.itemID).reminder
            items.append(Self.itemEntry(detail: detail, reminder: reminder))
        }
        var probes: [UpgradeReport.ProbeEntry] = []
        for itemID in request.probeItemIDs {
            do {
                _ = try await root.itemDetails.detail(itemID: itemID)
                probes.append(.init(itemID: itemID, state: "active"))
            } catch ItemDetailError.notFound {
                probes.append(.init(itemID: itemID, state: "not_found"))
            }
        }
        let listing = try await root.providerConfiguration.listProfiles()
        let routes = try await root.providerConfiguration.listRoutePolicy()
        let covered = Set(routes.flatMap(\.grantedCapabilities).flatMap(\.coveredProfileVersions))
        var providers: [UpgradeReport.ProviderEntry] = []
        for version in listing.profiles.flatMap(\.versions) {
            let status = (try? await root.providerCredentials.credentialStatus(profileVersion: version.profileVersion))
                .map(\.rawValue) ?? "unreadable"
            providers.append(
                .init(
                    profileVersion: version.profileVersion, providerProtocol: version.providerProtocol,
                    hasEndpoint: version.endpoint != nil, credentialBound: version.credentialBound,
                    credentialStatus: status, revoked: version.revoked, defaultFor: version.defaultFor,
                    routeCovered: covered.contains(version.profileVersion)))
        }
        let pending = await UNUserNotificationCenter.current().pendingNotificationRequests()
        var reminderRequests: [UpgradeReport.PendingReminderRequest] = []
        var otherPending = 0
        for request in pending {
            if let identifier = NotificationIdentifier(rawValue: request.identifier) {
                reminderRequests.append(
                    .init(reminderID: identifier.reminderID.rawValue, scheduleGeneration: Int(identifier.scheduleGeneration)))
            } else {
                otherPending += 1
            }
        }
        return UpgradeReport(
            schema: UpgradeReport.schema, generatedAt: ISO8601DateFormatter().string(from: Date()), build: build,
            recentItemCount: recent.count, items: items, probes: probes, providers: providers,
            pendingReminderRequests: reminderRequests.sorted { $0.reminderID < $1.reminderID },
            otherPendingRequestCount: otherPending)
    }

    static func itemEntry(detail: CoreItemDetail, reminder: ReminderRecordAnswer?) -> UpgradeReport.ItemEntry {
        UpgradeReport.ItemEntry(
            itemID: detail.itemID, revision: detail.revision, itemType: detail.itemType,
            lifecycleState: detail.lifecycleState, textBasis: detail.textBasis.rawValue,
            textLength: detail.text?.count ?? 0, originalTextLength: detail.originalText?.count,
            correctedTextLength: detail.correctedText?.count,
            reminder: reminder.map {
                UpgradeReport.ReminderEntry(
                    reminderID: $0.reminderID, requestState: $0.requestState, scheduleState: $0.scheduleState,
                    scheduleGeneration: $0.scheduleGeneration, resolvedInstant: $0.resolvedInstant)
            })
    }
}
#endif
