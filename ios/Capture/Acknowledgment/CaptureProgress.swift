import Foundation

/// How far audio has come toward text. Text captures are `notApplicable`.
enum CaptureTranscriptionProgress: Equatable {
    case notApplicable
    case pending
    case transcribed
    /// This device or configuration cannot transcribe this audio. The audio stays saved.
    case unavailable
    case failed
}

/// Whether the saved item has been read for a reminder yet.
enum CaptureInterpretationProgress: Equatable {
    case pending
    case processed
    /// Processing finished and chose not to guess.
    case abstained
    case uninterpreted
}

enum CaptureReminderBlock: Equatable {
    case timeInPast
    case permissionDenied
    case capacityExceeded
    case unsupportedRecurrence
    case other
}

/// What is known about the reminder. `installed` is the only case that may be worded as a reminder being set, and it
/// requires a resolved time: a schedule state without one is `notConfirmed`.
enum CaptureReminderProgress: Equatable {
    /// Nothing is confirmed yet: transcription or interpretation is still pending, or the report is not usable.
    case notConfirmed
    /// Processing finished and no reminder was asked for.
    case notRequested
    case notScheduled(CaptureReminderBlock)
    case installed(dueInstant: Date)
}

struct CaptureProgress: Equatable {
    var transcription: CaptureTranscriptionProgress
    var interpretation: CaptureInterpretationProgress
    var reminder: CaptureReminderProgress
}

/// Plain-string view of the core's independent status facts plus the resolved due time from the pending notification
/// request. Strings, not `ItemStatusReport`, because the app target does not import the core bridge; a composition
/// root or test adapter copies the report across.
struct CaptureItemStatus: Equatable {
    var processingState: String
    var transcriptionState: String
    var reminderRequestState: String?
    var reminderScheduleState: String?
    var unschedulableReason: String?
    /// The due instant of the installed notification request, when one exists. The core's item status carries no time.
    var installedDueInstant: Date?

    var progress: CaptureProgress {
        CaptureProgress(transcription: transcription, interpretation: interpretation, reminder: reminder)
    }

    private var transcription: CaptureTranscriptionProgress {
        switch transcriptionState {
        case "not_applicable": return .notApplicable
        case "transcribed": return .transcribed
        case "transcription_unsupported": return .unavailable
        case "transcription_failed": return .failed
        default: return .pending
        }
    }

    private var interpretation: CaptureInterpretationProgress {
        switch processingState {
        case "processed": return .processed
        case "abstained": return .abstained
        case "uninterpreted": return .uninterpreted
        default: return .pending
        }
    }

    private var reminder: CaptureReminderProgress {
        guard transcription == .notApplicable || transcription == .transcribed else { return .notConfirmed }
        switch reminderRequestState {
        case "not_requested":
            return interpretation == .pending ? .notConfirmed : .notRequested
        case "unschedulable":
            return .notScheduled(block)
        case "unsupported_recurrence":
            return .notScheduled(.unsupportedRecurrence)
        case "cancelled":
            return .notRequested
        case "resolved", "not_scheduled_yet":
            if reminderScheduleState == "scheduled", let dueInstant = installedDueInstant {
                return .installed(dueInstant: dueInstant)
            }
            if reminderScheduleState == "schedule_failed" { return .notScheduled(.other) }
            return .notConfirmed
        default:
            return .notConfirmed
        }
    }

    private var block: CaptureReminderBlock {
        switch unschedulableReason {
        case "time_in_past": return .timeInPast
        case "permission_denied": return .permissionDenied
        case "capacity_exceeded": return .capacityExceeded
        default: return .other
        }
    }
}

enum CaptureProgressReadResult: Equatable {
    case status(CaptureItemStatus)
    /// Nothing could be read. The acknowledgment keeps saying the reminder is not confirmed.
    case unavailable
}

/// Reads one saved item's status. Reading stored data needs an authenticated session, so the model asks only while
/// its access policy allows and drops answers that arrive after that stopped being true.
///
/// Confined to the main queue: `completion` is called there exactly once.
protocol CaptureProgressReading: AnyObject {
    func readStatus(itemID: String, completion: @escaping (CaptureProgressReadResult) -> Void)
}
