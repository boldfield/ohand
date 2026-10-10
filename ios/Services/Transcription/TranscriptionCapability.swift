import Foundation
import OhAndCoreBridge

/// The production assembly of offline transcription: the core client, the real on-device recognizer and the job
/// handler, registered with the job runner for the `transcription_attachment` job type.
///
/// The app's composition root builds one with `make(core:layout:)` and attaches the runner through
/// `attachJobRunner(...)`, so the handler runs queued recordings; it forwards every core event to `handle(_:)`
/// (after the job runner's own) and calls `shutDown()` before closing the core.
public struct TranscriptionCapability: Sendable {
    public let client: TranscriptionCoreClient
    public let handler: TranscriptionJobHandler
    public let recognizer: OnDeviceRecognizing

    init(client: TranscriptionCoreClient, handler: TranscriptionJobHandler, recognizer: OnDeviceRecognizing) {
        self.client = client
        self.handler = handler
        self.recognizer = recognizer
    }

    /// Assembles transcription with the real speech-framework recognizer, which recognizes on the device only.
    /// `mayPromptForPermission` answers `true` only while the app is in the foreground.
    static func make(
        core: CoreHandle,
        layout: ProtectedStorageLayout,
        mayPromptForPermission: @escaping @Sendable () -> Bool = { true },
        interpretation: @escaping @Sendable (TranscriptionJobSource) -> InterpretationJobRequest? = { _ in nil }
    ) -> TranscriptionCapability {
        make(
            core: core, layout: layout,
            recognizer: SpeechFrameworkRecognizer(mayPromptForPermission: mayPromptForPermission),
            interpretation: interpretation)
    }

    /// The same assembly with another recognizer, for tests that cannot make the speech framework produce speech.
    static func make(
        core: CoreHandle,
        layout: ProtectedStorageLayout,
        recognizer: OnDeviceRecognizing,
        interpretation: @escaping @Sendable (TranscriptionJobSource) -> InterpretationJobRequest? = { _ in nil }
    ) -> TranscriptionCapability {
        let client = TranscriptionCoreClient(core: core)
        let handler = TranscriptionJobHandler(
            core: client, recognizer: recognizer, layout: layout, interpretation: interpretation)
        return TranscriptionCapability(client: client, handler: handler, recognizer: recognizer)
    }

    /// Attaches the job runner with this capability registered, along with any `otherCapabilities`.
    public func attachJobRunner(
        to core: CoreHandle,
        storePath: String,
        sender: ProviderRequestSender,
        otherCapabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = false,
        retryDelay: TimeInterval = 60
    ) throws -> JobRunnerService {
        try JobRunnerService.attach(
            to: core, storePath: storePath, sender: sender, capabilities: [handler] + otherCapabilities,
            isReachable: isReachable, retryDelay: retryDelay)
    }

    /// Consumes a core event that answers one of the transcription client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        client.handle(event)
    }

    public func shutDown() {
        client.shutDown()
    }
}
