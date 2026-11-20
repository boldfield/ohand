import Foundation

/// How one on-device recognition attempt ended.
public enum OnDeviceRecognitionOutcome: Equatable, Sendable {
    /// Recognition finished. `text` may be empty when the audio held no speech; the core decides what that means.
    /// `confidence` is in `0...1` when the recognizer reports one.
    case transcript(text: String, confidence: Double?)
    /// The language or its on-device model is not available on this device. There is no remote fallback: the item
    /// stays pending and the audio is kept.
    case unsupported
    /// Recognition cannot run right now but may later (permission not yet decided, recognizer momentarily
    /// unavailable). `reason` is a short lowercase label (`[a-z0-9_]`).
    case unavailableNow(reason: String)
    /// The user denied speech recognition, or it is restricted on this device.
    case permissionDenied
    /// The audio could not be recognized.
    case failed
    /// The attempt was stopped because the calling task was cancelled.
    case cancelled
}

/// A speech recognizer that works on the device only. An implementation must never send audio to a remote service,
/// whatever the language or model availability.
public protocol OnDeviceRecognizing: AnyObject, Sendable {
    /// Name and version recorded with every transcript it produces; at most 128 bytes.
    var recognizerName: String { get }
    /// Which behaviour of this recognizer produced a transcript, sent with every transcript and stored by the core so
    /// a later, better recognizer can replace the text of notes an earlier one made. 1 is the recognizer that kept
    /// only the last segment it was given; 2 keeps every recognized segment. Raise it when the recognizer changes in a
    /// way that can change the text of audio it has already read.
    var transcriptGeneration: Int { get }
    /// Recognizes the audio file in `audioURL`, spoken in `localeIdentifier` (such as `en_US`). Honors task
    /// cancellation by returning `.cancelled` promptly.
    func recognize(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome
}
