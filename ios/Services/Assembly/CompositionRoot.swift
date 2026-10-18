import Foundation
import OhAndCoreBridge
import UIKit
import UserNotifications

// MARK: - Errors

/// Why the composition root could not be built. Cases carry fixed labels only, never paths or content.
public enum CompositionError: Error, Equatable {
    case unsupportedConfigurationVersion(Int)
    case invalidConfiguration(String)
    /// A store directory could not be created, so nothing could be persisted.
    case storageUnavailable
    case duplicateModule(String)
    /// A module's operation identifier range overlaps one the root (or another module) already uses.
    case operationRangeConflict(String)
    /// Two job capabilities claim the same job type.
    case jobTypeConflict(String)
    /// A core client with a fixed operation range was asked for a second time; two instances would share identifiers.
    case coreClientAlreadyTaken(String)
    /// The configured capture route could not be provisioned, so no save would be accepted. The label is the core's
    /// normalized failure code (for example `route_scope_mismatch`) or `route_provisioning_timeout`; never content.
    case captureRouteUnavailable(String)
}

// MARK: - Configuration

/// The versioned settings the production root is built from. Production construction reads only this value (and the
/// `CompositionEnvironment`) and picks the real adapters itself; it has no switch that selects a fake.
public struct CompositionConfiguration: Codable, Equatable, Sendable {
    /// The only configuration version this build understands. A newer or older file is refused rather than guessed at.
    public static let supportedVersion = 1

    public var version: Int
    /// The directory that holds every protected store. It is created if missing.
    public var storageRootPath: String
    /// The route and scope stamped on every capture the root saves.
    public var captureRouteID: String
    public var captureItemScope: String
    /// The Keychain service name of the provider credentials the native transport resolves at dispatch.
    public var credentialServiceName: String
    /// How long after a drain that left retryable work the job runner tries again while the app stays in the foreground.
    public var jobRetryDelaySeconds: Double

    public init(
        version: Int = CompositionConfiguration.supportedVersion,
        storageRootPath: String,
        captureRouteID: String,
        captureItemScope: String = "personal",
        credentialServiceName: String = CredentialService.defaultServiceName,
        jobRetryDelaySeconds: Double = 60
    ) {
        self.version = version
        self.storageRootPath = storageRootPath
        self.captureRouteID = captureRouteID
        self.captureItemScope = captureItemScope
        self.credentialServiceName = credentialServiceName
        self.jobRetryDelaySeconds = jobRetryDelaySeconds
    }

    enum CodingKeys: String, CodingKey {
        case version
        case storageRootPath = "storage_root_path"
        case captureRouteID = "capture_route_id"
        case captureItemScope = "capture_item_scope"
        case credentialServiceName = "credential_service_name"
        case jobRetryDelaySeconds = "job_retry_delay_seconds"
    }

    /// A file names the version, storage root and capture route; every other setting falls back to its default.
    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        version = try values.decode(Int.self, forKey: .version)
        storageRootPath = try values.decode(String.self, forKey: .storageRootPath)
        captureRouteID = try values.decode(String.self, forKey: .captureRouteID)
        captureItemScope = try values.decodeIfPresent(String.self, forKey: .captureItemScope) ?? "personal"
        credentialServiceName =
            try values.decodeIfPresent(String.self, forKey: .credentialServiceName)
            ?? CredentialService.defaultServiceName
        jobRetryDelaySeconds = try values.decodeIfPresent(Double.self, forKey: .jobRetryDelaySeconds) ?? 60
    }

    private struct VersionProbe: Decodable {
        let version: Int
    }

    /// Decodes and validates a configuration file. The version is read first, so a file from another version is
    /// reported as such even when its other fields have changed shape.
    public static func decode(_ data: Data) throws -> CompositionConfiguration {
        let probe = try JSONDecoder().decode(VersionProbe.self, from: data)
        guard probe.version == supportedVersion else {
            throw CompositionError.unsupportedConfigurationVersion(probe.version)
        }
        let configuration = try JSONDecoder().decode(CompositionConfiguration.self, from: data)
        try configuration.validate()
        return configuration
    }

    public func validate() throws {
        guard version == Self.supportedVersion else {
            throw CompositionError.unsupportedConfigurationVersion(version)
        }
        guard !storageRootPath.isEmpty else { throw CompositionError.invalidConfiguration("storage_root_path") }
        guard !captureRouteID.isEmpty else { throw CompositionError.invalidConfiguration("capture_route_id") }
        guard !captureItemScope.isEmpty else { throw CompositionError.invalidConfiguration("capture_item_scope") }
        guard !credentialServiceName.isEmpty else { throw CompositionError.invalidConfiguration("credential_service_name") }
        guard jobRetryDelaySeconds.isFinite, jobRetryDelaySeconds > 0 else {
            throw CompositionError.invalidConfiguration("job_retry_delay_seconds")
        }
    }
}

/// The clock, calendar context and observer a build supplies. These are not adapters: production and test builds both
/// take them, and the defaults are the real device values.
public struct CompositionEnvironment: Sendable {
    public var now: @Sendable () -> Date
    public var timeZone: @Sendable () -> TimeZone
    public var locale: @Sendable () -> Locale
    /// Receives the OS notification facts (delivered, opened, action) the reminder bridge ingests.
    public var notificationEvents: NotificationEventHandler

    public init(
        now: @escaping @Sendable () -> Date = { Date() },
        timeZone: @escaping @Sendable () -> TimeZone = { TimeZone.current },
        locale: @escaping @Sendable () -> Locale = { Locale.current },
        notificationEvents: @escaping NotificationEventHandler = { _ in }
    ) {
        self.now = now
        self.timeZone = timeZone
        self.locale = locale
        self.notificationEvents = notificationEvents
    }
}

// MARK: - Extension seam

/// What the root hands a module when it attaches. It deliberately carries no `CoreHandle`: the handle's read verbs are
/// ungated, so a module reaches the core only through `clients` (clients whose verbs never return stored history, and
/// that enforce their own authentication where they must) and `privateReads` (the gated read verbs). It carries no
/// storage location either, since the root's directory holds the core's protected stores, and no provider transport:
/// the transport sends whatever origins a caller claims, so a module reaches a provider only through the job
/// capabilities it contributes, which the job runner drives from the core's own route, grant and revocation decision,
/// and through `clients.shadowReview()`, whose requests the core builds and authorizes before the transport the root
/// attached to its own core handle sends them.
public struct CompositionContext {
    public let clients: CompositionCoreClients
    public let privateReads: CompositionPrivateReads
    public let now: @Sendable () -> Date
}

/// Builds the core-backed clients a module owns, without handing the module the core handle itself. Each client takes
/// the fixed operation identifier range it documents, not the module's `firstOperationID`; a module forwards events to
/// the client's `handle`. The root reserves every one of these ranges (`ServiceCompositionRoot.reservedOperationRanges`)
/// whether or not a module takes the client, so no module's own range can overlap one, and each client can be taken
/// once per root: a second request throws `coreClientAlreadyTaken`, because two instances would issue the same
/// identifiers and take each other's answers.
///
/// Only clients whose callers cannot claim a read authorization they do not hold belong here. The raw export and
/// metrics clients take the caller's word that reads are authenticated, and the raw notification action client
/// completes and resolves reminders without the authenticated session `ActionHandler` requires, so none of them is
/// offered: a capability that needs one is built over the root's read session, never from this factory.
public struct CompositionCoreClients {
    fileprivate let core: CoreHandle
    fileprivate let taken = TakenCoreClients()

    public func audioRetention() throws -> CoreAudioRetentionClient {
        try taken.take(ServiceCompositionRoot.audioRetentionClientName)
        return CoreAudioRetentionClient(core: core)
    }

    public func deletionCleanup() throws -> CoreDeletionCleanupClient {
        try taken.take(ServiceCompositionRoot.deletionCleanupClientName)
        return CoreDeletionCleanupClient(core: core)
    }

    public func processingHealth(
        stallAfterSeconds: UInt32 = 0, leaseGraceSeconds: UInt32 = 0
    ) throws -> ProcessingHealthService {
        try taken.take(ServiceCompositionRoot.processingHealthClientName)
        return ProcessingHealthService(
            core: core, stallAfterSeconds: stallAfterSeconds, leaseGraceSeconds: leaseGraceSeconds)
    }

    public func shadowReview(configuration: ShadowReviewConfiguration = .off) throws -> ShadowReviewService {
        try taken.take(ServiceCompositionRoot.shadowReviewClientName)
        return ShadowReviewService(core: core, configuration: configuration)
    }

    /// Reads whether a stored provider profile exists, is revoked, and serves a model. The answer carries no endpoint,
    /// credential or content, so it is not a gated read.
    public func providerProfileStatus() throws -> CoreProviderProfileStatusClient {
        try taken.take(ServiceCompositionRoot.providerProfileStatusClientName)
        return CoreProviderProfileStatusClient(core: core)
    }
}

/// The result of one provider-profile status read.
public typealias CoreProviderProfileStatusResult = Result<ProviderProfileStatus, CoreFailure>

/// Asks the core about stored provider profiles without handing a module the core handle. A module forwards each
/// event to `handle(_:)`, which returns `true` when the event answered one of this client's reads. Operation
/// identifiers start at `firstOperationID`. Call `shutDown()` to fail the reads still waiting.
public final class CoreProviderProfileStatusClient: @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 41

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the provider profile status client was shut down")
    private static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreProviderProfileStatusClient.firstOperationID
    private var pending: [UInt64: @Sendable (CoreProviderProfileStatusResult) -> Void] = [:]
    private var isShutDown = false

    init(core: CoreHandle) {
        self.core = core
    }

    /// Reads the stored profile `profileVersion` and calls `completion` once. A missing profile is a successful
    /// answer with `exists == false`.
    public func status(
        profileVersion: String, model: String,
        completion: @escaping @Sendable (CoreProviderProfileStatusResult) -> Void
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = completion
        lock.unlock()
        do {
            try core.startProviderProfileStatus(operationID: operationID, profileVersion: profileVersion, model: model)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?(.failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }

    /// Consumes the event if it answers one of this client's reads.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let completion = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let completion else { return false }
        switch event.outcome {
        case .failure(let failure):
            completion(.failure(failure))
        case .success:
            if let answer = try? event.decode(ProviderProfileStatus.self) {
                completion(.success(answer))
            } else {
                completion(.failure(Self.unreadableAnswer))
            }
        }
        return true
    }

    /// Fails every read still waiting and refuses later ones. Idempotent.
    public func shutDown() {
        lock.lock()
        isShutDown = true
        let waiting = pending
        pending = [:]
        lock.unlock()
        for completion in waiting.values {
            completion(.failure(Self.shutDownFailure))
        }
    }
}

/// Lets a service built during assembly reach the root that does not exist yet. The root sets itself once, at the end of
/// its initializer, before anything can call the service.
private final class WeakRootReference: @unchecked Sendable {
    private let lock = NSLock()
    private weak var storedRoot: ServiceCompositionRoot?

    var root: ServiceCompositionRoot? {
        lock.lock()
        defer { lock.unlock() }
        return storedRoot
    }

    func set(_ root: ServiceCompositionRoot) {
        lock.lock()
        storedRoot = root
        lock.unlock()
    }
}

/// The fixed-range core clients already handed out by one root's factory.
private final class TakenCoreClients {
    private let lock = NSLock()
    private var names = Set<String>()

    func take(_ name: String) throws {
        lock.lock()
        defer { lock.unlock() }
        guard names.insert(name).inserted else { throw CompositionError.coreClientAlreadyTaken(name) }
    }
}

/// The core's read verbs for stored history, state derived from it and the store check, behind the read gate. A read is
/// refused with an `unauthorized` failure while the session is capture-only, and a module must pass every event for
/// one of these operations through `admit` before decoding it: an answer that arrives after a relock is refused too.
public final class CompositionPrivateReads {
    private let gate: PrivateReadGate

    init(gate: PrivateReadGate) {
        self.gate = gate
    }

    public func startStoreCheck(operationID: UInt64) throws {
        try gated { try gate.startStoreCheck(operationID: operationID) }
    }

    public func startGetCapture(operationID: UInt64, captureID: String) throws {
        try gated { try gate.startGetCapture(operationID: operationID, captureID: captureID) }
    }

    public func startItemStatus(operationID: UInt64, itemID: String) throws {
        try gated { try gate.startItemStatus(operationID: operationID, itemID: itemID) }
    }

    /// Full-text search over the items the session may read. The answer decodes as `PrivateRetrievalAnswer`, after
    /// `admit`.
    public func startSearch(
        operationID: UInt64, query: String, filters: PrivateRetrievalFilters = PrivateRetrievalFilters()
    ) throws {
        try gated { try gate.startSearch(operationID: operationID, query: query, filters: filters) }
    }

    /// Every item the session may read that satisfies the filters. The answer
    /// decodes as `PrivateRetrievalAnswer`, after `admit`.
    public func startList(operationID: UInt64, filters: PrivateRetrievalFilters = PrivateRetrievalFilters()) throws {
        try gated { try gate.startList(operationID: operationID, filters: filters) }
    }

    /// Returns normally only while the read this event answers is still authorized. On a throw the caller discards the
    /// event without decoding it. Each answer is admitted at most once.
    public func admit(operationID: UInt64) throws {
        try gated { try gate.admit(operationID: operationID) }
    }

    private func gated(_ body: () throws -> Void) throws {
        do {
            try body()
        } catch let denial as PrivateReadDenied {
            throw CoreItemStatusReader.failure(for: denial)
        }
    }
}

/// The read scope of the running session, as the app sees it.
public enum CompositionReadScope: Equatable, Sendable {
    /// New captures can be saved; no stored history or state derived from it is returned.
    case captureOnly
    case authenticated
}

/// How one read-authentication attempt ended.
public enum CompositionAuthenticationOutcome: Equatable, Sendable {
    case authenticated
    case cancelled
    case denied
    /// No usable authentication method on the device.
    case unavailable
    /// A relock or a newer attempt ended this attempt before it could grant access.
    case interrupted
}

/// An optional capability that joins the root without editing it (the T11 seam). A module owns one core client with
/// its own operation identifier range, may contribute native job capabilities, and receives the core events that
/// answer its operations.
///
/// A module's own range is `[firstOperationID, firstOperationID + ServiceCompositionRoot.operationRangeWidth)`. The
/// root refuses a module whose `firstOperationID` is below `ServiceCompositionRoot.minimumModuleOperationID`, or whose
/// range overlaps one of `ServiceCompositionRoot.reservedOperationRanges` (the root's own clients, every fixed-range
/// client `CompositionCoreClients` can hand out, processing health's whole interval and the job runner's interval up to
/// the highest identifier) or another module's range, so two clients can never take each other's events. Modules are
/// attached in the order given, after the job runner.
public protocol CompositionModule: AnyObject {
    var identifier: String { get }
    var firstOperationID: UInt64 { get }
    /// Native job types this module runs; they are registered with the job host when it attaches.
    var jobCapabilities: [JobCapabilityHandler] { get }
    /// Provider protocols this module needs the core to dispatch, beyond the base ones (Anthropic and OpenAI). The
    /// root enables the union of every module's declaration; a protocol no module declares stays disabled (self-hosted
    /// is disabled unless a module names it).
    var enabledProviderProtocols: Set<CoreProviderProtocol> { get }
    /// The stored self-hosted profile versions and models this module lets the core send to. Enabling self-hosted
    /// is not enough: the root passes the union of every module's declaration to the core, which refuses any
    /// self-hosted request whose profile version and model are not named (default none).
    var selfHostedRegistrations: Set<CoreSelfHostedRegistration> { get }
    func attach(to context: CompositionContext) throws
    /// Takes the event if it answers one of this module's operations.
    func handle(_ event: CoreEvent) -> Bool
    /// Ends work still waiting. The root calls it before closing the core.
    func shutDown()
}

public extension CompositionModule {
    var jobCapabilities: [JobCapabilityHandler] { [] }
    var enabledProviderProtocols: Set<CoreProviderProtocol> { [] }
    var selfHostedRegistrations: Set<CoreSelfHostedRegistration> { [] }
}

/// A module that owns the ordered launch and foreground reconciliation (staged-capture recovery, job activation and
/// the reminder pass). When one is attached, `ServiceCompositionRoot.start(isActive:)` leaves those triggers to it, so
/// they run once, in its order, through its serialization, instead of also running unordered from the root's own
/// observers. The root keeps what is not ordered recovery work: foreground state, relocking, reachability, background
/// time and suspension. If several modules conform, the first one attached owns the lifecycle.
public protocol CompositionLifecycleOwner: CompositionModule {
    /// Connects the module to the built root before the root starts. Returns `false` when it cannot take the
    /// lifecycle, and the root then runs its own launch and foreground work as if no owner were attached.
    func connectLifecycle(to root: ServiceCompositionRoot) -> Bool
    /// Starts the module's launch and foreground triggers, in place of the root's own. Called on the main queue.
    func beginLifecycle(isActive: Bool)
}

// MARK: - Adapters

/// Which family of adapters a root was built with.
public enum CompositionAdapterSet: Equatable, Sendable {
    /// The real transport, notification center, reachability monitor and Keychain.
    case production
    /// Substitutes injected by a debug test build; never produced by production construction.
    case injectedForTests
}

/// The effectful boundaries the root connects. Only `ServiceCompositionRoot.productionAdapters(for:)` and the
/// debug-only `injectedForTests` factory can make one, so a release build has no path to a substitute.
struct CompositionAdapters {
    let kind: CompositionAdapterSet
    let sender: ProviderRequestSender
    /// Registers `sender` as the transport of the root's own core handle. Production uses the real coordinator; a
    /// debug test may substitute one that fails.
    let attachProviderTransport: (CoreHandle, ProviderRequestSender) throws -> ProviderExchangeCoordinator
    let notificationCenter: NotificationCenterProviding
    let permissionObservations: NotificationPermissionObservationStore
    let reachability: NetworkReachability
    let backgroundGrant: BackgroundExecutionGrant?
    let lifecycleNotifications: NotificationCenter
    /// `nil` selects the on-device speech recognizer.
    let recognizer: OnDeviceRecognizing?
    /// The read-authorization session every private read goes through.
    let readSession: ReadAuthenticationSession
    /// The Keychain-backed credentials the transport resolves at dispatch and provider setup enters secrets into.
    let credentials: CredentialService
    /// The reset core the reset service and control use, given the root's real client. Production uses the client
    /// itself; a debug test may wrap it to fail a read.
    let resetCore: (ResetCore) -> ResetCore

    fileprivate init(
        kind: CompositionAdapterSet,
        sender: ProviderRequestSender,
        attachProviderTransport: @escaping (CoreHandle, ProviderRequestSender) throws -> ProviderExchangeCoordinator,
        notificationCenter: NotificationCenterProviding,
        permissionObservations: NotificationPermissionObservationStore,
        reachability: NetworkReachability,
        backgroundGrant: BackgroundExecutionGrant?,
        lifecycleNotifications: NotificationCenter,
        recognizer: OnDeviceRecognizing?,
        readSession: ReadAuthenticationSession,
        credentials: CredentialService,
        resetCore: @escaping (ResetCore) -> ResetCore = { $0 }
    ) {
        self.kind = kind
        self.sender = sender
        self.attachProviderTransport = attachProviderTransport
        self.notificationCenter = notificationCenter
        self.permissionObservations = permissionObservations
        self.reachability = reachability
        self.backgroundGrant = backgroundGrant
        self.lifecycleNotifications = lifecycleNotifications
        self.recognizer = recognizer
        self.readSession = readSession
        self.credentials = credentials
        self.resetCore = resetCore
    }

    fileprivate static func production(for configuration: CompositionConfiguration) -> CompositionAdapters {
        let credentials = CredentialService(serviceName: configuration.credentialServiceName)
        return CompositionAdapters(
            kind: .production,
            sender: ProviderHTTPTransport(credentials: credentials),
            attachProviderTransport: { try ProviderExchangeCoordinator.attach(to: $0, sender: $1) },
            notificationCenter: SystemNotificationCenter(),
            permissionObservations: UserDefaultsNotificationPermissionObservationStore(),
            reachability: SystemNetworkReachability(),
            backgroundGrant: ApplicationBackgroundExecutionGrant(),
            lifecycleNotifications: .default,
            recognizer: nil,
            readSession: ReadAuthenticationSession(boundary: LocalAuthenticationBoundary()),
            credentials: credentials)
    }

    #if DEBUG
    /// Debug builds only: the substitutes a test injects. Compiled out of release builds. The session is required so a
    /// test states whether its reads are authenticated.
    static func injectedForTests(
        sender: ProviderRequestSender,
        attachProviderTransport: @escaping (CoreHandle, ProviderRequestSender) throws -> ProviderExchangeCoordinator = {
            try ProviderExchangeCoordinator.attach(to: $0, sender: $1)
        },
        readSession: ReadAuthenticationSession,
        notificationCenter: NotificationCenterProviding,
        permissionObservations: NotificationPermissionObservationStore = InMemoryNotificationPermissionObservationStore(),
        reachability: NetworkReachability,
        backgroundGrant: BackgroundExecutionGrant? = nil,
        lifecycleNotifications: NotificationCenter = NotificationCenter(),
        recognizer: OnDeviceRecognizing? = nil,
        credentials: CredentialService = CredentialService(
            serviceName: "com.boldfield.ohand.tests.composition.\(UUID().uuidString)"),
        resetCore: @escaping (ResetCore) -> ResetCore = { $0 }
    ) -> CompositionAdapters {
        CompositionAdapters(
            kind: .injectedForTests, sender: sender, attachProviderTransport: attachProviderTransport,
            notificationCenter: notificationCenter,
            permissionObservations: permissionObservations, reachability: reachability,
            backgroundGrant: backgroundGrant, lifecycleNotifications: lifecycleNotifications,
            recognizer: recognizer, readSession: readSession, credentials: credentials, resetCore: resetCore)
    }
    #endif
}

// MARK: - Public outcomes

/// What saving a text capture came to, in the words a capture surface needs.
public enum CompositionSaveOutcome: Equatable, Sendable {
    /// The core confirmed the import. The only outcome that may be shown as saved.
    case saved(captureID: String, itemID: String, alreadySaved: Bool)
    /// Durable on this device and waiting for the core to confirm.
    case keptOnDevice(captureID: String)
    /// Nothing durable was written; the caller still holds the input.
    case notStaged(captureID: String)
    case itemDeleted(captureID: String)
}

/// A finished recording offered to the root: the recorder's in-progress file and the finalized name bound to the
/// capture ID (it must start with `<captureID>.`).
public struct CompositionRecordingHandoff: Equatable, Sendable {
    public var captureID: String
    public var inProgressFileName: String
    public var finalizedFileName: String
    public var startedAt: Date

    public init(captureID: String, inProgressFileName: String, finalizedFileName: String, startedAt: Date) {
        self.captureID = captureID
        self.inProgressFileName = inProgressFileName
        self.finalizedFileName = finalizedFileName
        self.startedAt = startedAt
    }
}

/// What handing off a recording came to, with the same honesty rules as `CompositionSaveOutcome`.
public enum CompositionRecordingOutcome: Equatable, Sendable {
    /// The core confirmed the import. The only outcome that may be shown as saved.
    case saved(captureID: String, itemID: String, savedAt: String, alreadySaved: Bool)
    /// Durable on this device and waiting for the core to confirm.
    case keptOnDevice(captureID: String)
    /// Nothing was staged; the recording remains in the in-progress store.
    case notStaged(captureID: String)
    /// A record is staged, but the audio could not be moved into the finalized store (for example the in-progress file is
    /// missing or unreadable). Not saved, and nothing proves the audio is durable.
    case audioUnavailable(captureID: String)
    case itemDeleted(captureID: String)
}

/// Why a staged capture is not confirmed saved. The capture itself is durable on the device in every case.
public enum CompositionPendingReason: Equatable, Sendable {
    case importUnconfirmed
    case coreRejected(code: String)
    case audioUnavailable
    case stagingRecordUnreadable
}

public struct CompositionPendingCapture: Equatable, Sendable {
    public var captureID: String
    public var reason: CompositionPendingReason
}

/// What a restart recovery pass found.
public struct CompositionRecoveryReport: Equatable, Sendable {
    /// Staged captures whose import is still unconfirmed. They are retained, never reported as saved.
    public var pending: [CompositionPendingCapture] = []
    /// Staged captures the core confirmed during this pass.
    public var confirmedCaptureIDs: [String] = []
    /// Staged captures whose item the user had deleted; nothing was re-imported.
    public var deletedItemCaptureIDs: [String] = []
    /// Files in the in-progress audio store that no staged capture owns, left in place and never imported. nil means
    /// ownership could not be fully determined and must never be read as "none".
    public var unclaimedInProgressFileNames: [String]?
}

// MARK: - Event routing

/// Hands each core event to the clients in turn until one claims it. The core has one event handler, and every client
/// owns a distinct operation identifier range, so an event has exactly one claimant.
final class CoreEventRouter: @unchecked Sendable {
    private let lock = NSLock()
    private var consumers: [(CoreEvent) -> Bool] = []

    func register(_ consumer: @escaping (CoreEvent) -> Bool) {
        lock.lock()
        consumers.append(consumer)
        lock.unlock()
    }

    func route(_ event: CoreEvent) {
        lock.lock()
        let snapshot = consumers
        lock.unlock()
        for consumer in snapshot where consumer(event) {
            return
        }
    }
}

/// Adapts the core handle to the read gate's verbs.
private final class CoreHandleReadSource: PrivateReadSource {
    private let core: CoreHandle

    init(core: CoreHandle) {
        self.core = core
    }

    func startStoreCheck(operationID: UInt64) throws {
        try core.startStoreCheck(operationID: operationID)
    }

    func startGetCapture(operationID: UInt64, captureID: String) throws {
        try core.startGetCapture(operationID: operationID, captureID: captureID)
    }

    func startItemStatus(operationID: UInt64, itemID: String) throws {
        try core.startItemStatus(operationID: operationID, itemID: itemID)
    }

    func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {
        try core.startRetrieval(operationID: operationID, request: request)
    }

    func startReminderRead(
        operationID: UInt64, itemID: String, kind: ReminderReadKind, readScopes: [CoreExportScope]
    ) throws {
        try core.startReminderRead(operationID: operationID, itemID: itemID, kind: kind, readScopes: readScopes)
    }

    func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {
        try core.startItemDetail(operationID: operationID, itemID: itemID, readScopes: readScopes)
    }

    func startRecentItems(operationID: UInt64, readScopes: [CoreExportScope], limit: Int?, cursor: String?) throws {
        try core.startRecentItems(operationID: operationID, readScopes: readScopes, limit: limit, cursor: cursor)
    }

    func startRetentionRead(operationID: UInt64, kind: RetentionReadKind, readScopes: [CoreExportScope]) throws {
        try core.startRetentionRead(operationID: operationID, kind: kind, readScopes: readScopes)
    }

    func startJobConfigurationRead(operationID: UInt64, command: JobConfigurationCommand) throws {
        try core.startJobConfiguration(operationID: operationID, command: command)
    }
}

/// Reads an item's independent status facts from the core, only through the private read gate: a capture-only session
/// never reaches the core, and an answer that arrives after a relock is dropped undecoded.
final class CoreItemStatusReader: @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 57

    private static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreItemStatusReader.firstOperationID
    private var pending: [UInt64: (Result<ItemStatusReport, CoreFailure>) -> Void] = [:]
    private var isShutDown = false

    init(gate: PrivateReadGate) {
        self.gate = gate
    }

    /// The content-free failure a refused read reports; it never says whether the item exists.
    static func failure(for denial: PrivateReadDenied) -> CoreFailure {
        switch denial {
        case .sessionNotAuthenticated:
            return CoreFailure(
                errorClass: .unauthorized, code: "read_not_authenticated",
                message: "reading requires an authenticated session")
        case .relockedBeforeDelivery:
            return CoreFailure(
                errorClass: .unauthorized, code: "read_relocked", message: "the session relocked before the answer")
        case .notAGatedRead:
            return unreadableAnswer
        }
    }

    func read(itemID: String, completion: @escaping (Result<ItemStatusReport, CoreFailure>) -> Void) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(CoreFailure.handleClosed))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = completion
        lock.unlock()
        do {
            try gate.startItemStatus(operationID: operationID, itemID: itemID)
        } catch {
            lock.lock()
            let claimed = pending.removeValue(forKey: operationID)
            lock.unlock()
            if let denial = error as? PrivateReadDenied {
                claimed?(.failure(Self.failure(for: denial)))
            } else {
                claimed?(.failure((error as? CoreFailure) ?? CoreFailure.handleClosed))
            }
        }
    }

    func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let completion = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let completion else { return false }
        do {
            try gate.admit(operationID: event.operationID)
        } catch {
            let denial = (error as? PrivateReadDenied) ?? .notAGatedRead
            completion(.failure(Self.failure(for: denial)))
            return true
        }
        switch event.outcome {
        case .failure(let failure):
            completion(.failure(failure))
        case .success:
            if let report = try? event.decode(ItemStatusReport.self) {
                completion(.success(report))
            } else {
                completion(.failure(Self.unreadableAnswer))
            }
        }
        return true
    }

    func shutDown() {
        lock.lock()
        isShutDown = true
        let abandoned = pending
        pending = [:]
        lock.unlock()
        for completion in abandoned.values {
            completion(.failure(CoreFailure.handleClosed))
        }
    }
}

/// Whether the app is in the foreground, readable from any thread.
private final class ForegroundState: @unchecked Sendable {
    private let lock = NSLock()
    private var active = false

    var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return active
    }

    func set(_ value: Bool) {
        lock.lock()
        active = value
        lock.unlock()
    }
}

/// Holds the callback a resolved tap runs, so the app can set it after the root is built.
private final class OpenItemRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var handler: (@Sendable (String) -> Void)?

    func set(_ handler: (@Sendable (String) -> Void)?) {
        lock.lock()
        self.handler = handler
        lock.unlock()
    }

    func open(_ itemID: String) {
        lock.lock()
        let current = handler
        lock.unlock()
        current?(itemID)
    }
}

/// Holds the callback a prompt tap runs. A tap that arrives while none is set is held and handed over once, in order,
/// when one is set, so it is never dropped.
private final class PromptTapRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var handler: (@Sendable (PromptNotificationEvent) -> Void)?
    private var held: [PromptNotificationEvent] = []

    func set(_ newHandler: (@Sendable (PromptNotificationEvent) -> Void)?) {
        lock.lock()
        handler = newHandler
        let pending = newHandler == nil ? [] : held
        if newHandler != nil { held = [] }
        lock.unlock()
        for event in pending { newHandler?(event) }
    }

    func receive(_ event: PromptNotificationEvent) {
        guard event.kind == .opened else { return }
        lock.lock()
        guard let current = handler else {
            held.append(event)
            lock.unlock()
            return
        }
        lock.unlock()
        current(event)
    }
}

/// The notification-action pieces the root owns: the core client, the durable journal's handler and the delegate.
/// Nothing here is public; the root exposes the delegate, a reconcile and the open-item setter.
final class NotificationActionClients: @unchecked Sendable {
    static let journalDirectoryName = "NotificationActions"

    let client: CoreNotificationActionClient
    let handler: ActionHandler
    let ingestor: NotificationEventIngestor
    let receiver: NotificationEventReceiver
    fileprivate let openItem: OpenItemRelay
    fileprivate let promptTap = PromptTapRelay()
    private let scopeToken: SessionScopeObserverToken

    init(
        core: CoreHandle,
        layout: ProtectedStorageLayout,
        session: ReadAuthenticationSession,
        now: @escaping @Sendable () -> Date,
        forwardTo environmentHandler: @escaping NotificationEventHandler
    ) {
        let client = CoreNotificationActionClient(core: core)
        let journal = FileNotificationActionJournal(
            directory: layout.directory(for: .temporaryFiles)
                .appendingPathComponent(Self.journalDirectoryName, isDirectory: true))
        let relay = OpenItemRelay()
        let handler = ActionHandler(
            core: client, journal: journal, authorization: session, openItem: { relay.open($0) })
        let promptTap = promptTap
        let ingestor = NotificationEventIngestor(
            now: now,
            handler: { event in
                handler.eventHandler(event)
                environmentHandler(event)
            },
            promptHandler: { promptTap.receive($0) })
        self.client = client
        self.handler = handler
        self.ingestor = ingestor
        openItem = relay
        receiver = NotificationEventReceiver(ingestor: ingestor)
        scopeToken = handler.attach(to: session)
    }

    func shutDown(session: ReadAuthenticationSession) {
        session.removeScopeObserver(scopeToken)
        client.shutDown()
    }
}

/// Waits for the answer to the route-provisioning request. The core delivers events on the main queue, so a caller on
/// the main thread has to keep the run loop turning while it waits.
private final class RouteProvisioningWait: @unchecked Sendable {
    private let lock = NSLock()
    private let signal = DispatchSemaphore(value: 0)
    private var outcome: Result<Void, CoreFailure>?

    func handle(_ event: CoreEvent) -> Bool {
        guard event.operationID == ServiceCompositionRoot.routeProvisioningOperationID else { return false }
        let result: Result<Void, CoreFailure>
        switch event.outcome {
        case .failure(let failure):
            result = .failure(failure)
        case .success:
            if (try? event.decode(RouteProvisioningOutcome.self)) != nil {
                result = .success(())
            } else {
                result = .failure(
                    CoreFailure(
                        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read"))
            }
        }
        lock.lock()
        outcome = outcome ?? result
        lock.unlock()
        signal.signal()
        return true
    }

    func wait(timeout: TimeInterval) -> Result<Void, CoreFailure>? {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let answered = current { return answered }
            if Thread.isMainThread {
                RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
            } else {
                _ = signal.wait(timeout: .now() + 0.05)
            }
        }
        return current
    }

    private var current: Result<Void, CoreFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return outcome
    }
}

// MARK: - Root

/// The one place the production services are connected: the Rust core, the foreground ingress that saves into it, the
/// job runner with its authorized native provider transport, offline transcription, and the reminder scheduler.
///
/// * **One event handler.** The core delivers every event to a single handler. The root installs a router and gives
///   each client the events of its own operation identifier range, so no client has to own the handler.
/// * **Adapters.** `init(configuration:environment:modules:)` builds the real HTTPS transport over the Keychain, the
///   system notification center, the system reachability monitor and the application's background grant. A debug-only
///   `init(testing:...)` takes substitutes; there is no configuration value that selects one.
/// * **Reminders.** A finished drain asks the notification reconciler for a pass, so a reminder the core derived from an
///   interpretation reaches the scheduler without waiting for the next foreground.
/// * **Provider transport.** The root attaches the native provider transport to its own core handle during assembly
///   (and unregisters it on shutdown), so core-mediated exchanges such as shadow review can dispatch. Without it the
///   core refuses them with `transport_not_registered`.
/// * **Extension seam.** `CompositionModule`s attach after the job runner without any edit here.
///
/// Build it on the main queue and call `shutDown()` before releasing it: that stops the job runner, ends waiting
/// requests, unregisters the native effects and closes the core.
public final class ServiceCompositionRoot {
    /// The smallest first operation identifier a module may use; below it lie the capture importer's identifiers.
    public static let minimumModuleOperationID: UInt64 = 1 << 32
    /// The width of a module's own operation range and of each fixed-range client's reserved range.
    public static let operationRangeWidth: UInt64 = 1 << 40

    /// The route-provisioning request's identifier: the top of the importer's interval, which the importer never issues.
    static let routeProvisioningOperationID: UInt64 = minimumModuleOperationID - 1
    static let routeProvisioningTimeoutSeconds: TimeInterval = 30
    static let captureRouteName = "Local capture"

    static let audioRetentionClientName = "audio retention"
    static let deletionCleanupClientName = "deletion cleanup"
    static let processingHealthClientName = "processing health"
    static let shadowReviewClientName = "shadow review"
    static let providerProfileStatusClientName = "provider profile status"
    /// The provider protocols enabled for every root: today's behaviour, before any module declares more.
    static let baseProviderProtocols: Set<CoreProviderProtocol> = [.anthropic, .openAI]
    static let promptScheduleClientName = "prompt schedule"
    /// `ShadowReviewService` keeps its first operation identifier private; this is its value.
    static let shadowReviewFirstOperationID: UInt64 = 1 << 48
    static let itemCorrectionClientName = "item corrections"
    static let itemDeletionClientName = "item deletion"
    static let reminderControlClientName = "reminder controls"
    static let itemLifecycleControlClientName = "item lifecycle controls"
    static let suggestionResponseClientName = "suggestion responses"
    static let itemDetailClientName = "item detail"
    static let retentionControlClientName = "retention controls"
    static let jobConfigurationClientName = "job configuration"
    static let providerProfileClientName = "provider profiles"
    static let recentItemsClientName = "recent items"
    static let providerConnectionCheckClientName = "provider connection check"
    static let routePolicyClientName = "route policy"

    /// One half-open interval of operation identifiers `[start, end)` that a client claims.
    public struct OperationRange: Equatable, Sendable {
        public let name: String
        public let start: UInt64
        public let end: UInt64

        func overlaps(_ other: OperationRange) -> Bool {
            start < other.end && other.start < end
        }
    }

    private static func fixedWidthRange(_ name: String, from start: UInt64) -> OperationRange {
        OperationRange(name: name, start: start, end: start + operationRangeWidth)
    }

    /// Every interval a module's range may not overlap: the importer's identifiers below
    /// `minimumModuleOperationID`, the root's own clients, every fixed-range client a module can take from
    /// `CompositionCoreClients` or that another service builds over the core (export, notification actions, metrics),
    /// and the intervals whose `handle` claims every identifier in them rather than only those it issued: processing
    /// health up to the job runner, and the job runner up to the highest identifier.
    public static let reservedOperationRanges: [OperationRange] = [
        OperationRange(name: "importer", start: 0, end: minimumModuleOperationID),
        fixedWidthRange(shadowReviewClientName, from: shadowReviewFirstOperationID),
        fixedWidthRange(providerProfileStatusClientName, from: CoreProviderProfileStatusClient.firstOperationID),
        fixedWidthRange("transcription", from: TranscriptionCoreClient.firstOperationID),
        fixedWidthRange(deletionCleanupClientName, from: CoreDeletionCleanupClient.firstOperationID),
        fixedWidthRange("export", from: CoreExportClient.firstOperationID),
        fixedWidthRange("reset", from: CoreResetClient.firstOperationID),
        fixedWidthRange(promptScheduleClientName, from: CorePromptScheduleClient.firstOperationID),
        fixedWidthRange("reminders", from: CoreReminderReconciler.firstOperationID),
        fixedWidthRange("item status", from: CoreItemStatusReader.firstOperationID),
        fixedWidthRange(audioRetentionClientName, from: CoreAudioRetentionClient.firstOperationID),
        fixedWidthRange("metrics", from: CoreMetricsClient.firstOperationID),
        fixedWidthRange("notification actions", from: CoreNotificationActionClient.firstOperationID),
        fixedWidthRange(itemCorrectionClientName, from: CoreItemCorrectionClient.firstOperationID),
        fixedWidthRange(itemDeletionClientName, from: CoreItemDeletionClient.firstOperationID),
        fixedWidthRange(reminderControlClientName, from: CoreReminderControlClient.firstOperationID),
        fixedWidthRange(
            itemLifecycleControlClientName, from: CoreItemLifecycleControlClient.firstOperationID),
        fixedWidthRange(suggestionResponseClientName, from: CoreSuggestionResponseClient.firstOperationID),
        fixedWidthRange(itemDetailClientName, from: CoreItemDetailClient.firstOperationID),
        fixedWidthRange(retentionControlClientName, from: CoreRetentionControlClient.firstOperationID),
        fixedWidthRange(jobConfigurationClientName, from: CoreJobConfigurationClient.firstOperationID),
        fixedWidthRange(providerProfileClientName, from: CoreProviderProfileClient.firstOperationID),
        fixedWidthRange(providerConnectionCheckClientName, from: CoreProviderConnectionCheckClient.firstOperationID),
        fixedWidthRange(routePolicyClientName, from: CoreRoutePolicyClient.firstOperationID),
        fixedWidthRange(recentItemsClientName, from: CoreRecentItemsClient.firstOperationID),
        OperationRange(
            name: processingHealthClientName, start: ProcessingHealthService.firstOperationID,
            end: JobRunnerService.firstOperationID),
        OperationRange(name: "job runner", start: JobRunnerService.firstOperationID, end: UInt64.max),
    ]

    public let configuration: CompositionConfiguration
    public var adapterSet: CompositionAdapterSet { adapters.kind }
    public let jobRunner: JobRunnerService
    public let reminderReconciler: NotificationPermissionReconciler
    /// The optional daily prompt schedule. Settings calls `configure`; the lifecycle pass keeps it current once the
    /// root is started with a `LifecycleCoordinator`.
    public let promptSchedule: PromptScheduleService
    /// Corrects an item's text, type or session topic; needs an authenticated read session.
    public let itemCorrections: ItemCorrectionService
    /// Deletes one item and runs its native cleanup at once; needs an authenticated read session.
    public let itemDeletion: ItemDeletionService
    /// Reads, edits, cancels and reviews an item's reminder; reads are gated and writes need an authenticated session.
    public let reminderControls: ReminderControlService
    /// Completes, cancels or quiets an item; needs an authenticated read session.
    public let itemLifecycleControls: ItemLifecycleControlService
    /// Opens a daily prompt and records the user's answer; needs an authenticated read session.
    public let suggestionResponses: SuggestionResponseService
    /// Reads one item's full detail; needs an authenticated read session.
    public let itemDetails: ItemDetailService
    /// Lists the most recent captures; needs an authenticated read session.
    public let recentItems: RecentItemsService
    /// Configures audio retention and acts on unresolved sole audio; reads are gated and writes need an authenticated
    /// session.
    public let retentionControls: RetentionControlService
    /// Opens an item's retained audio for playback without exposing protected storage; needs an authenticated read
    /// session. Open playback holds the audio in place and ends on deletion, relock and shutdown.
    public let playback: PlaybackService
    /// Re-pins waiting work, requeues a job, revokes a profile and lists the jobs pinned to one; needs an authenticated
    /// read session.
    public let jobConfiguration: JobConfigurationService
    /// Creates provider profiles, sets defaults, checks a connection on request and grants or revokes route processing;
    /// every call needs an authenticated read session.
    public let providerConfiguration: ProviderConfigurationService
    /// Stores, replaces, deletes and reports the status of a provider secret, never returning one; every call needs an
    /// authenticated read session.
    public let providerCredentials: ProviderCredentialEntry
    /// Prepares an export for the share sheet; needs an authenticated read session. Leftover export files are swept at
    /// launch, after capture recovery has been started.
    public let exportService: ExportService
    /// Resets all captured content and reads the reset's progress; needs an authenticated read session. New captures
    /// pass the reset ingress gate, so one made during a reset waits and is imported afterwards, never lost. An
    /// interrupted reset is resumed at launch, before the job runner activates.
    public let resetControl: ResetControl

    let adapters: CompositionAdapters
    /// The session whose scope gates every private read the root performs.
    var readSession: ReadAuthenticationSession { adapters.readSession }
    let coreHandle: CoreHandle
    let transcription: TranscriptionCapability
    /// The ingestor the delegate hands OS callbacks to; tests feed callbacks through it, since a
    /// `UNNotificationResponse` cannot be built outside the OS.
    var notificationIngestor: NotificationEventIngestor { notificationActions.ingestor }

    private let environment: CompositionEnvironment
    private let layout: ProtectedStorageLayout
    private let protectionFailureTotal: Int
    private let statusReader: CoreItemStatusReader
    private let reminderCore: CoreReminderReconciler
    private let promptScheduleClient: CorePromptScheduleClient
    private let providerTransport: ProviderExchangeCoordinator
    private let notificationActions: NotificationActionClients
    private let itemCorrectionClient: CoreItemCorrectionClient
    private let itemDeletionClient: CoreItemDeletionClient
    private let reminderControlClient: CoreReminderControlClient
    private let itemLifecycleControlClient: CoreItemLifecycleControlClient
    private let suggestionResponseClient: CoreSuggestionResponseClient
    private let itemDetailClient: CoreItemDetailClient
    private let recentItemsClient: CoreRecentItemsClient
    private let retentionControlClient: CoreRetentionControlClient
    private let jobConfigurationClient: CoreJobConfigurationClient
    private let providerProfileClient: CoreProviderProfileClient
    private let providerConnectionCheckClient: CoreProviderConnectionCheckClient
    private let routePolicyClient: CoreRoutePolicyClient
    private let exportClient: CoreExportClient
    private let resetClient: CoreResetClient
    private let resetStaging: NativeResetIngress
    private let resetItemCleanup: ResetItemCleanupBridge
    private let modules: [CompositionModule]
    private let foreground: ForegroundState
    private let stateLock = NSLock()
    private var ingress: ForegroundIngressService?
    private var isShutDown = false
    private var hasStarted = false
    private var jobObserver: JobRunnerLifecycleObserver?
    private var permissionObserver: NotificationPermissionLifecycleObserver?
    private var foregroundTokens: [NSObjectProtocol] = []
    private let inFlightReminderPasses = InFlightWork()
    private var reminderPassTasks: [Task<Void, Never>] = []
    private var latestReminderReport: NotificationPermissionReport?
    private var reminderReportObserver: (@Sendable (NotificationPermissionReport) -> Void)?

    /// Builds the production root: real adapters chosen from `configuration`.
    public convenience init(
        configuration: CompositionConfiguration,
        environment: CompositionEnvironment = CompositionEnvironment(),
        modules: [CompositionModule] = []
    ) throws {
        try self.init(
            configuration: configuration, environment: environment,
            adapters: CompositionAdapters.production(for: configuration), modules: modules)
    }

    #if DEBUG
    /// Debug builds only: the same assembly over injected substitutes.
    convenience init(
        testing configuration: CompositionConfiguration,
        environment: CompositionEnvironment = CompositionEnvironment(),
        adapters: CompositionAdapters,
        modules: [CompositionModule] = []
    ) throws {
        try self.init(configuration: configuration, environment: environment, adapters: adapters, modules: modules)
    }
    #endif

    private init(
        configuration: CompositionConfiguration,
        environment: CompositionEnvironment,
        adapters: CompositionAdapters,
        modules: [CompositionModule]
    ) throws {
        let assembly = try Self.assemble(
            configuration: configuration, environment: environment, adapters: adapters, modules: modules)
        self.configuration = configuration
        self.environment = environment
        self.adapters = adapters
        self.modules = modules
        layout = assembly.layout
        protectionFailureTotal = assembly.protectionFailureTotal
        coreHandle = assembly.core
        ingress = assembly.ingress
        statusReader = assembly.statusReader
        jobRunner = assembly.jobRunner
        transcription = assembly.transcription
        reminderCore = assembly.reminderCore
        promptScheduleClient = assembly.promptScheduleClient
        promptSchedule = assembly.promptSchedule
        providerTransport = assembly.providerTransport
        notificationActions = assembly.notificationActions
        itemCorrectionClient = assembly.itemCorrectionClient
        itemCorrections = assembly.itemCorrections
        itemDeletionClient = assembly.itemDeletionClient
        itemDeletion = assembly.itemDeletion
        reminderControlClient = assembly.reminderControlClient
        reminderControls = assembly.reminderControls
        itemLifecycleControlClient = assembly.itemLifecycleControlClient
        itemLifecycleControls = assembly.itemLifecycleControls
        suggestionResponseClient = assembly.suggestionResponseClient
        suggestionResponses = assembly.suggestionResponses
        itemDetailClient = assembly.itemDetailClient
        itemDetails = assembly.itemDetails
        recentItemsClient = assembly.recentItemsClient
        recentItems = assembly.recentItems
        retentionControlClient = assembly.retentionControlClient
        retentionControls = assembly.retentionControls
        playback = assembly.playback
        jobConfigurationClient = assembly.jobConfigurationClient
        jobConfiguration = assembly.jobConfiguration
        providerProfileClient = assembly.providerProfileClient
        providerConnectionCheckClient = assembly.providerConnectionCheckClient
        routePolicyClient = assembly.routePolicyClient
        providerConfiguration = assembly.providerConfiguration
        providerCredentials = assembly.providerCredentials
        exportClient = assembly.exportClient
        exportService = assembly.exportService
        resetClient = assembly.resetClient
        resetStaging = assembly.resetStaging
        resetControl = assembly.resetControl
        resetItemCleanup = assembly.resetItemCleanup
        reminderReconciler = assembly.permissionReconciler
        foreground = assembly.foreground
        assembly.rootReference.set(self)
        jobRunner.setDrainIdleObserver { [weak self] in self?.runReminderPassAfterDrain() }
        itemDeletion.setCleanupProvider { [weak self] in
            let coordinator = self?.modules.lazy.compactMap { $0 as? LifecycleCoordinator }.first
            let cleanup: ItemDeletionCleanup? = coordinator?.deletionCleanup
            return cleanup
        }
        resetItemCleanup.setProvider { [weak self] in
            self?.modules.lazy.compactMap { $0 as? LifecycleCoordinator }.first?.deletionCleanup
        }
    }

    deinit {
        shutDown()
    }

    // MARK: Reset

    /// Finishes a reset a previous run left unfinished. The lifecycle coordinator calls it at launch, before the job
    /// runner activates. It needs no authenticated session: it only completes what the user already confirmed, and
    /// does nothing when no reset is recorded.
    func resumeInterruptedReset() async -> ResetOutcome {
        await resetControl.resumeInterrupted()
    }

    // MARK: Observation

    /// Protection or backup attributes the platform refused when the stores were prepared. The stores still work and
    /// keep the class they inherited; the caller decides what to surface.
    public var storageProtectionFailureCount: Int { protectionFailureTotal }

    /// The most recent reminder pass this root ran, or `nil` before the first.
    public var lastReminderReport: NotificationPermissionReport? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return latestReminderReport
    }

    // MARK: Lifecycle

    /// Registers the launch, foreground, reachability and suspension hooks and recovers captures a previous run left
    /// staged. With `isActive` the launch drain and the launch reminder pass run at once. Call it once, on the main
    /// queue.
    ///
    /// When a `CompositionLifecycleOwner` module is attached (the `LifecycleCoordinator`), the recovery, job
    /// activation and reminder work of launch and of every foreground activation belongs to it alone and runs as its
    /// single serialized pass; the root then registers no observer of its own for them. Reachability, background
    /// time, suspension, relocking and foreground state stay with the root. Without an owner the root runs that work
    /// itself, unordered.
    public func start(isActive: Bool) {
        stateLock.lock()
        guard !isShutDown, !hasStarted else {
            stateLock.unlock()
            return
        }
        hasStarted = true
        let lifecycleOwner = modules.lazy.compactMap { $0 as? CompositionLifecycleOwner }.first
        let ownsLifecycle = lifecycleOwner?.connectLifecycle(to: self) ?? false
        if ownsLifecycle, let coordinator = lifecycleOwner as? LifecycleCoordinator {
            coordinator.setAction(promptSchedule.lifecycleAction, for: .promptEligibility)
        }
        let center = adapters.lifecycleNotifications
        let foregroundState = foreground
        foregroundState.set(isActive)
        foregroundTokens.append(
            center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
                foregroundState.set(true)
            })
        foregroundTokens.append(
            center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { _ in
                foregroundState.set(false)
            })
        let readSession = adapters.readSession
        for (name, reason) in [
            (UIApplication.didEnterBackgroundNotification, RelockReason.enteredBackground),
            (UIApplication.willEnterForegroundNotification, RelockReason.willEnterForeground),
            (UIApplication.protectedDataWillBecomeUnavailableNotification, RelockReason.protectedDataUnavailable),
        ] {
            foregroundTokens.append(
                center.addObserver(forName: name, object: nil, queue: nil) { _ in
                    readSession.relock(reason)
                })
        }
        let jobObserver = JobRunnerLifecycleObserver(
            service: jobRunner, notificationCenter: center, reachability: adapters.reachability,
            backgroundGrant: adapters.backgroundGrant, drivesActivation: !ownsLifecycle)
        let permissionObserver: NotificationPermissionLifecycleObserver? =
            ownsLifecycle
            ? nil
            : NotificationPermissionLifecycleObserver(
                reconciler: reminderReconciler, notificationCenter: center,
                onReport: { [weak self] report in self?.recordReminderReport(report) })
        self.jobObserver = jobObserver
        self.permissionObserver = permissionObserver
        let recovering = ownsLifecycle ? nil : ingress
        stateLock.unlock()

        recovering?.recover { _ in }
        jobObserver.start(isActive: isActive)
        permissionObserver?.start(isActive: isActive)
        if ownsLifecycle {
            lifecycleOwner?.beginLifecycle(isActive: isActive)
        }
        // After capture recovery has been started, and off the main queue so it never delays a capture.
        let exportService = self.exportService
        DispatchQueue.global(qos: .utility).async { exportService.sweepAtLaunch() }
    }

    /// Whether the app is active right now. Read it on the main queue, where the lifecycle notifications that change it
    /// are delivered, when the answer must be ordered against a background transition.
    var isAppActive: Bool { foreground.isActive }

    #if DEBUG
    /// Debug builds only: stands in for the activation state of a root a test does not start.
    func setAppActiveForTests(_ active: Bool) {
        foreground.set(active)
    }
    #endif

    /// Imports every capture a previous run left staged and reports what was found; `nil` once the root is shut down.
    /// Safe to repeat. Call on the main queue; the completion arrives there.
    func recoverStagedIngress(completion: @escaping (IngressRecoveryReport?) -> Void) {
        stateLock.lock()
        let service = ingress
        stateLock.unlock()
        guard let service else {
            completion(nil)
            return
        }
        service.recover { completion($0) }
    }

    /// Stops everything the root started and closes the core. Idempotent. Captures still waiting for the core stay
    /// staged on disk and are imported by the next run. Must not be called from a core callback.
    public func shutDown() {
        stateLock.lock()
        guard !isShutDown else {
            stateLock.unlock()
            return
        }
        isShutDown = true
        let jobObserver = self.jobObserver
        let permissionObserver = self.permissionObserver
        let tokens = foregroundTokens
        self.jobObserver = nil
        self.permissionObserver = nil
        foregroundTokens = []
        ingress = nil
        let reminderPasses = reminderPassTasks
        reminderPassTasks = []
        stateLock.unlock()

        reminderPasses.forEach { $0.cancel() }
        jobObserver?.stop()
        permissionObserver?.stop()
        tokens.forEach { adapters.lifecycleNotifications.removeObserver($0) }
        jobRunner.setDrainIdleObserver(nil)
        for module in modules.reversed() {
            module.shutDown()
        }
        providerTransport.shutDown()
        jobRunner.shutDown()
        transcription.shutDown()
        notificationActions.shutDown(session: adapters.readSession)
        itemCorrectionClient.shutDown()
        itemDeletion.setCleanupProvider(nil)
        itemDeletionClient.shutDown()
        reminderControlClient.shutDown()
        itemLifecycleControlClient.shutDown()
        suggestionResponseClient.shutDown()
        itemDetailClient.shutDown()
        recentItemsClient.shutDown()
        itemDeletion.setIntentObserver(nil)
        playback.shutDown()
        retentionControlClient.shutDown()
        jobConfigurationClient.shutDown()
        providerProfileClient.shutDown()
        providerConnectionCheckClient.shutDown()
        routePolicyClient.shutDown()
        exportClient.shutDown()
        resetStaging.retire()
        resetClient.shutDown()
        reminderCore.invalidate()
        promptScheduleClient.shutDown()
        statusReader.shutDown()
        coreHandle.close()
    }

    // MARK: Capture and status

    /// Saves a text capture: staged durably first, then imported into the core in one transaction. Call and completion
    /// are on the main queue. Saving the same `captureID` again converges on the same item.
    public func saveText(
        captureID: String,
        text: String,
        capturedAt: Date,
        completion: @escaping (CompositionSaveOutcome) -> Void
    ) {
        stateLock.lock()
        let service = ingress
        stateLock.unlock()
        guard let service else {
            completion(.notStaged(captureID: captureID))
            return
        }
        let record = IngressRecord(
            captureID: captureID, text: text, audio: nil, context: captureContext(at: capturedAt))
        service.submit(record) { outcome in
            completion(Self.saveOutcome(for: outcome))
        }
    }

    /// The directory the recorder must write in-progress audio to. Recordings handed off from here are moved, never
    /// copied, into the finalized audio store.
    public var inProgressAudioDirectory: URL { layout.directory(for: .ingressInProgressAudio) }

    /// Saves a finished recording: staged durably first, finalized by move, then imported into the core in one
    /// transaction. Call and completion are on the main queue. Handing off the same capture again converges on the
    /// same item.
    public func saveRecording(
        _ handoff: CompositionRecordingHandoff,
        completion: @escaping (CompositionRecordingOutcome) -> Void
    ) {
        stateLock.lock()
        let service = ingress
        stateLock.unlock()
        guard let service else {
            completion(.notStaged(captureID: handoff.captureID))
            return
        }
        let record = IngressRecord(
            captureID: handoff.captureID, text: nil,
            audio: IngressAudioHandoff(
                inProgressFileName: handoff.inProgressFileName, finalizedFileName: handoff.finalizedFileName),
            context: captureContext(at: handoff.startedAt))
        service.submit(record) { outcome in
            completion(Self.recordingOutcome(for: outcome))
        }
    }

    /// Imports every capture a previous run left staged and reports it; `nil` once the root is shut down. Safe to
    /// repeat. Call on the main queue; the completion arrives there.
    public func recoverRecordings(completion: @escaping (CompositionRecoveryReport?) -> Void) {
        recoverStagedIngress { report in
            completion(report.map(Self.recoveryReport(from:)))
        }
    }

    // MARK: Read authentication

    /// Whether private reads are currently allowed. Every launch starts `captureOnly`.
    public var readScope: CompositionReadScope {
        adapters.readSession.scope == .authenticated ? .authenticated : .captureOnly
    }

    /// Asks the device owner to authenticate. Only a success moves the scope to `authenticated`; any other outcome
    /// leaves (or returns) it `captureOnly`. `completion` runs on the main queue.
    public func authenticateReads(
        reason: String,
        completion: @escaping (CompositionAuthenticationOutcome) -> Void
    ) {
        adapters.readSession.authenticate(reason: reason) { result in
            switch result {
            case .authenticated:
                completion(.authenticated)
            case .notAuthenticated(.cancelled):
                completion(.cancelled)
            case .notAuthenticated(.denied):
                completion(.denied)
            case .notAuthenticated(.unavailable):
                completion(.unavailable)
            case .notAuthenticated(.interrupted):
                completion(.interrupted)
            }
        }
    }

    /// Makes one quiet biometrics-only attempt (no passcode fallback, no error shown) if the session is capture-only
    /// and no attempt is already in flight. Success authenticates the session as `authenticateReads` does; any other
    /// outcome leaves it capture-only. Never blocks capture.
    /// `completion` runs on the main queue only when an attempt was started, and says whether it authenticated.
    public func requestQuietReadAuthentication(reason: String, completion: @escaping (Bool) -> Void = { _ in }) {
        adapters.readSession.authenticateQuietly(reason: reason) { result in
            completion(result == .authenticated)
        }
    }

    /// Returns to capture-only and invalidates every read still waiting for its answer. The root also relocks itself
    /// when the app enters the background or returns to the foreground, and when protected data becomes unavailable.
    public func relockReads() {
        adapters.readSession.relock(.explicit)
    }

    /// Reads an item's save, processing, transcription and reminder facts from the core. Private: with a capture-only
    /// session the read is refused with an `unauthorized` failure and never reaches the core.
    public func itemStatus(
        itemID: String,
        completion: @escaping (Result<ItemStatusReport, CoreFailure>) -> Void
    ) {
        statusReader.read(itemID: itemID, completion: completion)
    }

    // MARK: Notification actions

    /// The delegate to install on `UNUserNotificationCenter`. It stores each delivered, opened and action callback
    /// durably before anything else happens, so a callback is not lost while the core, the network or a provider is
    /// unavailable. Completing and opening wait for an authenticated session.
    public var notificationCenterDelegate: UNUserNotificationCenterDelegate { notificationActions.receiver }

    /// Delivers the callbacks stored by this or an earlier launch that the session now allows. Call it at launch; the
    /// root repeats it by itself whenever the session authenticates.
    public func reconcileNotificationActions() async {
        _ = await notificationActions.handler.reconcile()
    }

    /// Sets (or clears, with `nil`) what runs when a tap resolves to an item. It receives the identifier the core
    /// resolved for the notification, never the payload's target, and only while the session that admitted the tap is
    /// still authenticated. It may run on any thread; set it before installing the delegate, since a tap that
    /// resolves while none is set is spent.
    public func setOpenItemHandler(_ handler: (@Sendable (String) -> Void)?) {
        notificationActions.openItem.set(handler)
    }

    /// Sets (or clears, with `nil`) what runs when a tapped daily prompt asks to open its suggestion. It receives the
    /// opened prompt event and may run on any thread. A tap that arrives while none is set is held and delivered once
    /// when one is set.
    public func setOpenSuggestionHandler(_ handler: (@Sendable (PromptNotificationEvent) -> Void)?) {
        notificationActions.promptTap.set(handler)
    }

    // MARK: Reminders

    /// Sets (or clears, with `nil`) what runs when a reminder pass produces a report different from the previous one,
    /// including when reminders become undeliverable because permission was never requested
    /// (`ReminderUndeliverableReason.permissionNotRequested`), so the app can ask then. It may run on any thread.
    public func setReminderReportObserver(_ observer: (@Sendable (NotificationPermissionReport) -> Void)?) {
        stateLock.lock()
        reminderReportObserver = observer
        stateLock.unlock()
    }

    /// Runs one reminder pass now: the core decides what to install or cancel, and the notification bridge performs it.
    @discardableResult
    public func reconcileReminders(
        trigger: NotificationReconcileTrigger = .explicit
    ) async throws -> NotificationPermissionReport {
        let report = try await reminderReconciler.refresh(trigger: trigger)
        recordReminderReport(report)
        return report
    }

    private func runReminderPassAfterDrain() {
        stateLock.lock()
        let stopped = isShutDown
        stateLock.unlock()
        guard !stopped else { return }
        let reconciler = reminderReconciler
        let inFlight = inFlightReminderPasses
        inFlight.begin()
        let pass = Task { [weak self] in
            defer { inFlight.end() }
            if Task.isCancelled { return }
            if let report = try? await reconciler.refresh(trigger: .explicit) {
                self?.recordReminderReport(report)
            }
        }
        stateLock.lock()
        reminderPassTasks.append(pass)
        stateLock.unlock()
    }

    /// Returns once no reminder pass started after a job drain is executing. Await it after `shutDown()` when nothing
    /// may still be calling into the core.
    func waitForReminderPasses() async {
        await inFlightReminderPasses.waitUntilIdle()
    }

    private func recordReminderReport(_ report: NotificationPermissionReport) {
        stateLock.lock()
        let changed = latestReminderReport != report
        latestReminderReport = report
        let observer = changed ? reminderReportObserver : nil
        stateLock.unlock()
        observer?(report)
    }

    // MARK: Helpers

    private func captureContext(at capturedAt: Date) -> IngressCaptureContext {
        let timeZone = environment.timeZone()
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        let stamp = formatter.string(from: capturedAt)
        return IngressCaptureContext(
            captureInstant: stamp,
            timezoneID: timeZone.identifier,
            utcOffsetMinutes: timeZone.secondsFromGMT(for: capturedAt) / 60,
            locale: environment.locale().identifier,
            calendar: "gregorian",
            itemScope: configuration.captureItemScope,
            routeID: configuration.captureRouteID,
            entryLocked: false,
            createdAt: stamp,
            sessionTopic: nil)
    }

    private static func saveOutcome(for outcome: IngressOutcome) -> CompositionSaveOutcome {
        switch outcome {
        case .saved(let acknowledgment):
            return .saved(
                captureID: acknowledgment.captureID, itemID: acknowledgment.itemID,
                alreadySaved: acknowledgment.alreadyImported)
        case .keptForRetry(let captureID, _):
            return .keptOnDevice(captureID: captureID)
        case .notStaged(let captureID, _):
            return .notStaged(captureID: captureID)
        case .itemDeleted(let captureID):
            return .itemDeleted(captureID: captureID)
        }
    }

    private static func recordingOutcome(for outcome: IngressOutcome) -> CompositionRecordingOutcome {
        switch outcome {
        case .saved(let acknowledgment):
            return .saved(
                captureID: acknowledgment.captureID, itemID: acknowledgment.itemID, savedAt: acknowledgment.savedAt,
                alreadySaved: acknowledgment.alreadyImported)
        case .keptForRetry(let captureID, .sourceUnavailable):
            return .audioUnavailable(captureID: captureID)
        case .keptForRetry(let captureID, _):
            return .keptOnDevice(captureID: captureID)
        case .notStaged(let captureID, _):
            return .notStaged(captureID: captureID)
        case .itemDeleted(let captureID):
            return .itemDeleted(captureID: captureID)
        }
    }

    private static func recoveryReport(from ingress: IngressRecoveryReport) -> CompositionRecoveryReport {
        var report = CompositionRecoveryReport()
        for entry in ingress.entries {
            switch entry.outcome {
            case .saved:
                report.confirmedCaptureIDs.append(entry.captureID)
            case .itemDeleted:
                report.deletedItemCaptureIDs.append(entry.captureID)
            case .keptForRetry(_, let problem):
                report.pending.append(
                    CompositionPendingCapture(captureID: entry.captureID, reason: pendingReason(for: problem)))
            case .notStaged:
                report.pending.append(
                    CompositionPendingCapture(captureID: entry.captureID, reason: .audioUnavailable))
            }
        }
        let ownershipKnown = ingress.listingFailure == nil && ingress.unclaimedAudioListingFailure == nil
            && !ingress.unclaimedAudioNotEvaluated
        report.unclaimedInProgressFileNames = ownershipKnown ? ingress.unclaimedInProgressAudio : nil
        return report
    }

    private static func pendingReason(for problem: IngressPendingProblem) -> CompositionPendingReason {
        switch problem {
        case .notCommitted, .commitUnknown, .coreUnavailable, .conflictingReuse: return .importUnconfirmed
        case .rejected(let code): return .coreRejected(code: code)
        case .sourceUnavailable: return .audioUnavailable
        case .recordUnreadable: return .stagingRecordUnreadable
        }
    }

    // MARK: Assembly

    private struct Assembly {
        let layout: ProtectedStorageLayout
        let protectionFailureTotal: Int
        let core: CoreHandle
        let ingress: ForegroundIngressService
        let statusReader: CoreItemStatusReader
        let jobRunner: JobRunnerService
        let transcription: TranscriptionCapability
        let reminderCore: CoreReminderReconciler
        let promptScheduleClient: CorePromptScheduleClient
        let promptSchedule: PromptScheduleService
        let rootReference: WeakRootReference
        let providerTransport: ProviderExchangeCoordinator
        let notificationActions: NotificationActionClients
        let itemCorrectionClient: CoreItemCorrectionClient
        let itemCorrections: ItemCorrectionService
        let itemDeletionClient: CoreItemDeletionClient
        let itemDeletion: ItemDeletionService
        let reminderControlClient: CoreReminderControlClient
        let reminderControls: ReminderControlService
        let itemLifecycleControlClient: CoreItemLifecycleControlClient
        let itemLifecycleControls: ItemLifecycleControlService
        let suggestionResponseClient: CoreSuggestionResponseClient
        let suggestionResponses: SuggestionResponseService
        let itemDetailClient: CoreItemDetailClient
        let itemDetails: ItemDetailService
        let recentItemsClient: CoreRecentItemsClient
        let recentItems: RecentItemsService
        let retentionControlClient: CoreRetentionControlClient
        let retentionControls: RetentionControlService
        let playback: PlaybackService
        let jobConfigurationClient: CoreJobConfigurationClient
        let jobConfiguration: JobConfigurationService
        let providerProfileClient: CoreProviderProfileClient
        let providerConnectionCheckClient: CoreProviderConnectionCheckClient
        let routePolicyClient: CoreRoutePolicyClient
        let providerConfiguration: ProviderConfigurationService
        let providerCredentials: ProviderCredentialEntry
        let exportClient: CoreExportClient
        let exportService: ExportService
        let resetClient: CoreResetClient
        let resetStaging: NativeResetIngress
        let resetControl: ResetControl
        let resetItemCleanup: ResetItemCleanupBridge
        let permissionReconciler: NotificationPermissionReconciler
        let foreground: ForegroundState
    }

    private static func validate(modules: [CompositionModule]) throws {
        var identifiers = Set<String>()
        var claimed = reservedOperationRanges
        for module in modules {
            guard identifiers.insert(module.identifier).inserted else {
                throw CompositionError.duplicateModule(module.identifier)
            }
            let start = module.firstOperationID
            // A start this high overlaps the job runner's reserved interval anyway; refusing it first keeps the end
            // below from overflowing.
            guard start < JobRunnerService.firstOperationID else {
                throw CompositionError.operationRangeConflict(module.identifier)
            }
            let range = OperationRange(name: module.identifier, start: start, end: start + operationRangeWidth)
            guard !claimed.contains(where: { $0.overlaps(range) }) else {
                throw CompositionError.operationRangeConflict(module.identifier)
            }
            claimed.append(range)
        }
    }

    /// Makes sure the configured capture route exists before ingress can accept or recover a save. A new route has no
    /// processing destinations and no authorizations, so provisioning never authorizes an upload. Blocks until the core
    /// answers, which takes a turning main run loop when called on the main thread.
    private static func provisionCaptureRoute(
        core: CoreHandle, router: CoreEventRouter, configuration: CompositionConfiguration
    ) throws {
        let wait = RouteProvisioningWait()
        router.register { wait.handle($0) }
        do {
            try core.startEnsureLocalCaptureRoute(
                operationID: routeProvisioningOperationID, routeID: configuration.captureRouteID,
                routeName: captureRouteName, scope: configuration.captureItemScope)
        } catch let failure as CoreFailure {
            throw CompositionError.captureRouteUnavailable(failure.code)
        }
        switch wait.wait(timeout: routeProvisioningTimeoutSeconds) {
        case .success?:
            return
        case .failure(let failure)?:
            throw CompositionError.captureRouteUnavailable(failure.code)
        case nil:
            throw CompositionError.captureRouteUnavailable("route_provisioning_timeout")
        }
    }

    private static func assemble(
        configuration: CompositionConfiguration,
        environment: CompositionEnvironment,
        adapters: CompositionAdapters,
        modules: [CompositionModule]
    ) throws -> Assembly {
        try configuration.validate()
        try validate(modules: modules)

        let layout = ProtectedStorageLayout(
            rootDirectory: URL(fileURLWithPath: configuration.storageRootPath, isDirectory: true))
        let protection = ProtectedStorageService(layout: layout)
        let report = protection.prepare()
        guard report.outcomes.allSatisfy({ $0.directoryAvailable }) else {
            throw CompositionError.storageUnavailable
        }

        let core = try CoreHandle(path: layout.databaseURL.path)
        var undo: [() -> Void] = [{ core.close() }]
        do {
            let router = CoreEventRouter()
            try core.setEventHandler { [router] event in router.route(event) }

            try provisionCaptureRoute(core: core, router: router, configuration: configuration)

            let importer = CoreIngressImporter(attachedTo: core)
            router.register { importer.handle($0) }
            // A recorded reset tap is read here, so captures made after a tap whose process died are held from launch.
            let resetMarkers = FileResetMarkerStore(layout: layout)
            let resetGate = ResetIngressGate(wrapping: importer, layout: layout, markers: resetMarkers)
            let ingress = try ForegroundIngressService(layout: layout, importer: resetGate, protection: protection)

            let readGate = PrivateReadGate(session: adapters.readSession, source: CoreHandleReadSource(core: core))
            let statusReader = CoreItemStatusReader(gate: readGate)
            router.register { statusReader.handle($0) }
            undo.append { statusReader.shutDown() }

            let bridge = NotificationBridge(
                center: adapters.notificationCenter,
                ingestor: NotificationEventIngestor(now: environment.now, handler: environment.notificationEvents),
                now: environment.now)
            let reminderCore = try CoreReminderReconciler(
                core: core, bridge: bridge, center: adapters.notificationCenter)
            undo.append { reminderCore.invalidate() }
            router.register { reminderCore.handle($0) }

            let promptScheduleClient = CorePromptScheduleClient(core: core)
            undo.append { promptScheduleClient.shutDown() }
            router.register { promptScheduleClient.handle($0) }
            let rootReference = WeakRootReference()
            let promptSchedule = PromptScheduleService(
                core: promptScheduleClient, timeZone: environment.timeZone,
                install: { trigger in
                    guard let root = rootReference.root else { throw CancellationError() }
                    return try await root.reconcileReminders(trigger: trigger)
                })

            let notificationActions = NotificationActionClients(
                core: core, layout: layout, session: adapters.readSession, now: environment.now,
                forwardTo: environment.notificationEvents)
            undo.append { notificationActions.shutDown(session: adapters.readSession) }
            router.register { notificationActions.client.handle($0) }
            let itemCorrectionClient = CoreItemCorrectionClient(core: core)
            router.register { itemCorrectionClient.handle($0) }
            undo.append { itemCorrectionClient.shutDown() }
            let itemCorrections = ItemCorrectionService(core: itemCorrectionClient, session: adapters.readSession)
            let itemDeletionClient = CoreItemDeletionClient(core: core, gate: readGate)
            router.register { itemDeletionClient.handle($0) }
            undo.append { itemDeletionClient.shutDown() }
            let itemDeletion = ItemDeletionService(core: itemDeletionClient, session: adapters.readSession)
            let reminderControlClient = CoreReminderControlClient(core: core, gate: readGate)
            router.register { reminderControlClient.handle($0) }
            undo.append { reminderControlClient.shutDown() }
            let reminderControls = ReminderControlService(core: reminderControlClient, session: adapters.readSession)
            let itemLifecycleControlClient = CoreItemLifecycleControlClient(core: core)
            router.register { itemLifecycleControlClient.handle($0) }
            undo.append { itemLifecycleControlClient.shutDown() }
            let itemLifecycleControls = ItemLifecycleControlService(
                core: itemLifecycleControlClient, session: adapters.readSession)
            let suggestionResponseClient = CoreSuggestionResponseClient(core: core, gate: readGate)
            router.register { suggestionResponseClient.handle($0) }
            undo.append { suggestionResponseClient.shutDown() }
            let suggestionResponses = SuggestionResponseService(
                core: suggestionResponseClient, session: adapters.readSession)
            let itemDetailClient = CoreItemDetailClient(gate: readGate)
            router.register { itemDetailClient.handle($0) }
            undo.append { itemDetailClient.shutDown() }
            let itemDetails = ItemDetailService(core: itemDetailClient)
            let recentItemsClient = CoreRecentItemsClient(gate: readGate)
            router.register { recentItemsClient.handle($0) }
            undo.append { recentItemsClient.shutDown() }
            let recentItems = RecentItemsService(core: recentItemsClient)
            let retentionControlClient = CoreRetentionControlClient(core: core, gate: readGate)
            router.register { retentionControlClient.handle($0) }
            undo.append { retentionControlClient.shutDown() }
            let retentionControls = RetentionControlService(
                core: retentionControlClient, session: adapters.readSession,
                pressureProbe: VolumeStoragePressureProbe(volumeURL: layout.rootDirectory),
                removerProvider: { modules.lazy.compactMap { ($0 as? LifecycleCoordinator)?.audioRetention }.first })
            let playback = PlaybackService(
                core: retentionControlClient, session: adapters.readSession, layout: layout, fence: PlaybackFence(),
                playerFactory: avPlaybackPlayerFactory)
            undo.append { playback.shutDown() }
            itemDeletion.setIntentObserver { [playback] itemID in playback.endPlayback(itemID: itemID) }
            let jobConfigurationClient = CoreJobConfigurationClient(core: core, gate: readGate)
            router.register { jobConfigurationClient.handle($0) }
            undo.append { jobConfigurationClient.shutDown() }
            let jobConfiguration = JobConfigurationService(
                core: jobConfigurationClient, session: adapters.readSession)
            let providerProfileClient = CoreProviderProfileClient(core: core)
            router.register { providerProfileClient.handle($0) }
            undo.append { providerProfileClient.shutDown() }
            let providerConnectionCheckClient = CoreProviderConnectionCheckClient(core: core)
            router.register { providerConnectionCheckClient.handle($0) }
            undo.append { providerConnectionCheckClient.shutDown() }
            let routePolicyClient = CoreRoutePolicyClient(core: core)
            router.register { routePolicyClient.handle($0) }
            undo.append { routePolicyClient.shutDown() }
            let providerConfiguration = ProviderConfigurationService(
                profiles: providerProfileClient, connectionChecks: providerConnectionCheckClient,
                routePolicy: routePolicyClient, session: adapters.readSession)
            let providerCredentials = ProviderCredentialEntry(
                store: adapters.credentials, session: adapters.readSession, profiles: providerProfileClient)
            let exportClient = CoreExportClient(core: core)
            router.register { exportClient.handle($0) }
            undo.append { exportClient.shutDown() }
            let exportService = ExportService(core: exportClient, session: adapters.readSession, layout: layout)
            let resetClient = CoreResetClient(core: core)
            router.register { resetClient.handle($0) }
            undo.append { resetClient.shutDown() }
            let resetItemCleanup = ResetItemCleanupBridge()
            let resetStaging = NativeResetIngress(service: ingress, gate: resetGate, layout: layout)
            undo.append { resetStaging.retire() }
            let resetCore = adapters.resetCore(resetClient)
            let resetService = ResetService(
                core: resetCore,
                staging: resetStaging,
                itemCleanup: resetItemCleanup,
                exportTemporaries: ExportTemporaryCleaner(layout: layout),
                credentials: KeychainResetCredentialRemover(serviceName: configuration.credentialServiceName),
                markers: resetMarkers)
            let resetControl = ResetControl(service: resetService, core: resetCore, session: adapters.readSession)
            let permissionReconciler = NotificationPermissionReconciler(
                center: adapters.notificationCenter, reconciliation: reminderCore,
                observations: adapters.permissionObservations)

            let foreground = ForegroundState()
            let transcription: TranscriptionCapability
            if let recognizer = adapters.recognizer {
                transcription = TranscriptionCapability.make(core: core, layout: layout, recognizer: recognizer)
            } else {
                transcription = TranscriptionCapability.make(
                    core: core, layout: layout, mayPromptForPermission: { foreground.isActive })
            }
            undo.append { transcription.shutDown() }
            router.register { transcription.handle($0) }

            var capabilities: [JobCapabilityHandler] = [transcription.handler]
            for module in modules {
                capabilities.append(contentsOf: module.jobCapabilities)
            }
            var jobTypes = Set<String>()
            for capability in capabilities where !jobTypes.insert(capability.jobType).inserted {
                throw CompositionError.jobTypeConflict(capability.jobType)
            }
            var enabledProtocols = ServiceCompositionRoot.baseProviderProtocols
            var selfHostedRegistrations = Set<CoreSelfHostedRegistration>()
            for module in modules {
                enabledProtocols.formUnion(module.enabledProviderProtocols)
                selfHostedRegistrations.formUnion(module.selfHostedRegistrations)
            }
            try core.setEnabledProviderProtocols(enabledProtocols)
            undo.append { try? core.setEnabledProviderProtocols(ServiceCompositionRoot.baseProviderProtocols) }
            try core.setSelfHostedRegistrations(selfHostedRegistrations)
            undo.append { try? core.setSelfHostedRegistrations([]) }
            let jobRunner = try JobRunnerService.attach(
                to: core, storePath: layout.databaseURL.path, sender: adapters.sender,
                capabilities: capabilities, isReachable: adapters.reachability.isReachable,
                retryDelay: configuration.jobRetryDelaySeconds)
            undo.append { jobRunner.shutDown() }
            router.register { jobRunner.handle($0) }

            let providerTransport = try adapters.attachProviderTransport(core, adapters.sender)
            undo.append { providerTransport.shutDown() }

            let context = CompositionContext(
                clients: CompositionCoreClients(core: core), privateReads: CompositionPrivateReads(gate: readGate),
                now: environment.now)
            for module in modules {
                try module.attach(to: context)
                undo.append { module.shutDown() }
                router.register { module.handle($0) }
            }

            return Assembly(
                layout: layout, protectionFailureTotal: report.failures.count, core: core, ingress: ingress,
                statusReader: statusReader, jobRunner: jobRunner, transcription: transcription,
                reminderCore: reminderCore, promptScheduleClient: promptScheduleClient,
                promptSchedule: promptSchedule, rootReference: rootReference, providerTransport: providerTransport,
                notificationActions: notificationActions, itemCorrectionClient: itemCorrectionClient,
                itemCorrections: itemCorrections, itemDeletionClient: itemDeletionClient,
                itemDeletion: itemDeletion, reminderControlClient: reminderControlClient,
                reminderControls: reminderControls, itemLifecycleControlClient: itemLifecycleControlClient,
                itemLifecycleControls: itemLifecycleControls,
                suggestionResponseClient: suggestionResponseClient,
                suggestionResponses: suggestionResponses, itemDetailClient: itemDetailClient,
                itemDetails: itemDetails, recentItemsClient: recentItemsClient, recentItems: recentItems,
                retentionControlClient: retentionControlClient,
                retentionControls: retentionControls, playback: playback,
                jobConfigurationClient: jobConfigurationClient,
                jobConfiguration: jobConfiguration, providerProfileClient: providerProfileClient,
                providerConnectionCheckClient: providerConnectionCheckClient, routePolicyClient: routePolicyClient,
                providerConfiguration: providerConfiguration, providerCredentials: providerCredentials,
                exportClient: exportClient, exportService: exportService,
                resetClient: resetClient, resetStaging: resetStaging, resetControl: resetControl, resetItemCleanup: resetItemCleanup,
                permissionReconciler: permissionReconciler, foreground: foreground)
        } catch {
            for step in undo.reversed() {
                step()
            }
            throw error
        }
    }
}
