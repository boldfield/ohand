import Foundation

/// One text capture handed to ingress. `captureID` is the idempotency key: submitting the same capture again, after an
/// interruption or a repeated tap, must converge on the same saved item.
struct TextCaptureSubmission: Equatable {
    var captureID: String
    /// Exactly what the user typed, including whitespace and line breaks.
    var text: String
    var capturedAt: Date
}

/// What ingress reported for a submission. Only `saved` may be presented as saved.
enum TextCaptureOutcome: Equatable {
    /// The core confirmed the durable import.
    case saved(captureID: String, itemID: String, alreadySaved: Bool)
    /// The text is durable in the native staging record but the import is not confirmed yet.
    case keptOnDevice(captureID: String)
    /// Nothing durable was written; the caller still holds the text and must keep it.
    case notStaged(captureID: String)
    /// The user deleted the item for this capture, so nothing is saved again.
    case itemDeleted(captureID: String)
}

/// The seam between the capture surface and native ingress. A conforming adapter maps the foreground ingress
/// service's outcomes one to one: a confirmed import is `saved`; a staged record whose import is unconfirmed is
/// `keptOnDevice`; a failure to stage is `notStaged`. It fills the routing and entry context (scope, route, time
/// zone, locale) from the user's configured defaults, so the surface never asks for them.
///
/// Confined to the main queue: `submitText` is called there and `completion` is called there exactly once.
protocol TextCaptureIngress: AnyObject {
    func submitText(_ submission: TextCaptureSubmission, completion: @escaping (TextCaptureOutcome) -> Void)
}
