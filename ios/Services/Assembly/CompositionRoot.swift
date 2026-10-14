import Foundation
import OhAndCoreBridge
import UIKit

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
    func attach(to context: CompositionContext) throws
    /// Takes the event if it answers one of this module's operations.
    func handle(_ event: CoreEvent) -> Bool
    /// Ends work still waiting. The root calls it before closing the core.
    func shutDown()
}

public extension CompositionModule {
    var jobCapabilities: [JobCapabilityHandler] { [] }
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
        readSession: ReadAuthenticationSession
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
            readSession: ReadAuthenticationSession(boundary: LocalAuthenticationBoundary()))
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
        recognizer: OnDeviceRecognizing? = nil
    ) -> CompositionAdapters {
        CompositionAdapters(
            kind: .injectedForTests, sender: sender, attachProviderTransport: attachProviderTransport,
            notificationCenter: notificationCenter,
            permissionObservations: permissionObservations, reachability: reachability,
            backgroundGrant: backgroundGrant, lifecycleNotifications: lifecycleNotifications,
            recognizer: recognizer, readSession: readSession)
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

    static let audioRetentionClientName = "audio retention"
    static let deletionCleanupClientName = "deletion cleanup"
    static let processingHealthClientName = "processing health"
    static let shadowReviewClientName = "shadow review"
    /// `ShadowReviewService` keeps its first operation identifier private; this is its value.
    static let shadowReviewFirstOperationID: UInt64 = 1 << 48

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
        fixedWidthRange("transcription", from: TranscriptionCoreClient.firstOperationID),
        fixedWidthRange(deletionCleanupClientName, from: CoreDeletionCleanupClient.firstOperationID),
        fixedWidthRange("export", from: CoreExportClient.firstOperationID),
        fixedWidthRange("reminders", from: CoreReminderReconciler.firstOperationID),
        fixedWidthRange("item status", from: CoreItemStatusReader.firstOperationID),
        fixedWidthRange(audioRetentionClientName, from: CoreAudioRetentionClient.firstOperationID),
        fixedWidthRange("metrics", from: CoreMetricsClient.firstOperationID),
        fixedWidthRange("notification actions", from: CoreNotificationActionClient.firstOperationID),
        OperationRange(
            name: processingHealthClientName, start: ProcessingHealthService.firstOperationID,
            end: JobRunnerService.firstOperationID),
        OperationRange(name: "job runner", start: JobRunnerService.firstOperationID, end: UInt64.max),
    ]

    public let configuration: CompositionConfiguration
    public var adapterSet: CompositionAdapterSet { adapters.kind }
    public let jobRunner: JobRunnerService
    public let reminderReconciler: NotificationPermissionReconciler

    let adapters: CompositionAdapters
    /// The session whose scope gates every private read the root performs.
    var readSession: ReadAuthenticationSession { adapters.readSession }
    let coreHandle: CoreHandle
    let transcription: TranscriptionCapability

    private let environment: CompositionEnvironment
    private let layout: ProtectedStorageLayout
    private let protectionFailureTotal: Int
    private let statusReader: CoreItemStatusReader
    private let reminderCore: CoreReminderReconciler
    private let providerTransport: ProviderExchangeCoordinator
    private let modules: [CompositionModule]
    private let foreground: ForegroundState
    private let stateLock = NSLock()
    private var ingress: ForegroundIngressService?
    private var isShutDown = false
    private var hasStarted = false
    private var jobObserver: JobRunnerLifecycleObserver?
    private var permissionObserver: NotificationPermissionLifecycleObserver?
    private var foregroundTokens: [NSObjectProtocol] = []
    private var latestReminderReport: NotificationPermissionReport?

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
        providerTransport = assembly.providerTransport
        reminderReconciler = assembly.permissionReconciler
        foreground = assembly.foreground
        jobRunner.setDrainIdleObserver { [weak self] in self?.runReminderPassAfterDrain() }
    }

    deinit {
        shutDown()
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
        stateLock.unlock()

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
        reminderCore.invalidate()
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

    // MARK: Reminders

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
        Task { [weak self] in
            if let report = try? await reconciler.refresh(trigger: .explicit) {
                self?.recordReminderReport(report)
            }
        }
    }

    private func recordReminderReport(_ report: NotificationPermissionReport) {
        stateLock.lock()
        latestReminderReport = report
        stateLock.unlock()
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
        let providerTransport: ProviderExchangeCoordinator
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

            let importer = CoreIngressImporter(attachedTo: core)
            router.register { importer.handle($0) }
            let ingress = try ForegroundIngressService(layout: layout, importer: importer, protection: protection)

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
                reminderCore: reminderCore, providerTransport: providerTransport,
                permissionReconciler: permissionReconciler, foreground: foreground)
        } catch {
            for step in undo.reversed() {
                step()
            }
            throw error
        }
    }
}
