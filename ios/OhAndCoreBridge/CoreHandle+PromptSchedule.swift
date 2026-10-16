import Foundation
import OhandCoreC

/// The user's choice for the optional daily prompt. Local wall-clock time; the core interprets it in the zone the caller
/// supplies with each request.
public enum PromptScheduleSetting: Equatable, Sendable {
    case off
    case daily(hour: Int, minute: Int)
}

/// What causes the core to look at the schedule again. Only `launch` refills the planning horizon; the others only drop
/// occurrences that are no longer valid. A change of the setting is a configure call, not a refresh.
public enum PromptScheduleRefreshTrigger: String, Encodable, Equatable, Sendable {
    case launch
    case foreground
    case timeOrZoneChange = "time_or_zone_change"
}

/// The next planned prompt occurrence. It carries no content: prompts are generic.
public struct PromptScheduleNextOccurrence: Decodable, Equatable, Sendable {
    /// The local calendar date the occurrence belongs to, as `YYYY-MM-DD`.
    public let localDate: String
    /// The instant the occurrence fires, as an RFC 3339 UTC timestamp.
    public let fireAt: String

    enum CodingKeys: String, CodingKey {
        case localDate = "local_date"
        case fireAt = "fire_at"
    }
}

/// The persisted prompt schedule as the core holds it. The outcome event of every prompt schedule operation decodes as
/// this type.
public struct PromptScheduleOutcome: Decodable, Equatable, Sendable {
    public struct Time: Decodable, Equatable, Sendable {
        public let hour: Int
        public let minute: Int
    }

    public let operationID: UInt64
    /// Whether daily prompting is on.
    public let enabled: Bool
    /// The chosen local time, or `nil` when prompting is off.
    public let time: Time?
    /// The soonest planned occurrence still ahead of the core clock, or `nil` when there is none.
    public let next: PromptScheduleNextOccurrence?

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case enabled
        case time
        case next
    }
}

private struct PromptScheduleConfigureRequest: Encodable {
    let timeZone: String
    let setting: PromptScheduleSetting

    enum CodingKeys: String, CodingKey {
        case timeZone = "time_zone"
        case settings
    }

    enum SettingKeys: String, CodingKey {
        case mode
        case time
    }

    enum TimeKeys: String, CodingKey {
        case hour
        case minute
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(timeZone, forKey: .timeZone)
        var settings = container.nestedContainer(keyedBy: SettingKeys.self, forKey: .settings)
        switch setting {
        case .off:
            try settings.encode("off", forKey: .mode)
        case .daily(let hour, let minute):
            try settings.encode("daily", forKey: .mode)
            var time = settings.nestedContainer(keyedBy: TimeKeys.self, forKey: .time)
            try time.encode(hour, forKey: .hour)
            try time.encode(minute, forKey: .minute)
        }
    }
}

private struct PromptScheduleRefreshRequest: Encodable {
    let timeZone: String
    let trigger: PromptScheduleRefreshTrigger

    enum CodingKeys: String, CodingKey {
        case timeZone = "time_zone"
        case trigger
    }
}

extension CoreHandle {
    /// Queues the user's choice for the optional daily prompt. `timeZoneIdentifier` is the device's current IANA zone
    /// (for example `TimeZone.current.identifier`); the core reads its own clock. The outcome event for `operationID`
    /// decodes as `PromptScheduleOutcome` and reflects the schedule as persisted, or is a failure whose `code` is
    /// `invalid_request` for an unknown zone or a time outside 00:00-23:59. Throws when the request could not be
    /// queued, in which case nothing ran.
    ///
    /// Configuring never installs anything: prompts are installed by the reminder reconciliation pass, which takes
    /// them only from the persisted schedule.
    public func startConfigurePromptSchedule(
        operationID: UInt64,
        setting: PromptScheduleSetting,
        timeZoneIdentifier: String
    ) throws {
        let request = PromptScheduleConfigureRequest(timeZone: timeZoneIdentifier, setting: setting)
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_configure_prompt_schedule(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues a look at the schedule for `trigger`: launch, foreground, or a time or zone change. The outcome event for
    /// `operationID` decodes as `PromptScheduleOutcome`, or is an `invalid_request` failure for an unknown zone. Throws
    /// when the request could not be queued.
    public func startRefreshPromptSchedule(
        operationID: UInt64,
        trigger: PromptScheduleRefreshTrigger,
        timeZoneIdentifier: String
    ) throws {
        let request = PromptScheduleRefreshRequest(timeZone: timeZoneIdentifier, trigger: trigger)
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_refresh_prompt_schedule(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues a read of the persisted schedule and its next planned occurrence. Reading changes nothing. The outcome
    /// event for `operationID` decodes as `PromptScheduleOutcome`. Throws when the request could not be queued.
    public func startReadPromptSchedule(operationID: UInt64) throws {
        let rawResult = ohand_core_start_read_prompt_schedule(handleIdentifier, operationID)
        _ = try consumeCoreResult(rawResult)
    }
}
