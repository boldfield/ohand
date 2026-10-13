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
/// that enforce their own authentication where they must) and `privateReads` (the gated read verbs).
public struct CompositionContext {
    public let clients: CompositionCoreClients
    public let privateReads: CompositionPrivateReads
    public let storageRootURL: URL
    public let sender: ProviderRequestSender
    public let now: @Sendable () -> Date
}

/// Builds the core-backed clients a module owns, without handing the module the core handle itself. Each client takes
/// the operation identifier range it documents; a module forwards the events of that range to the client's `handle`.
public struct CompositionCoreClients {
    fileprivate let core: CoreHandle

    public func audioRetention() -> CoreAudioRetentionClient { CoreAudioRetentionClient(core: core) }
    public func deletionCleanup() -> CoreDeletionCleanupClient { CoreDeletionCleanupClient(core: core) }
    public func export() -> CoreExportClient { CoreExportClient(core: core) }
    public func metrics() -> CoreMetricsClient { CoreMetricsClient(core: core) }
    public func notificationActions() -> CoreNotificationActionClient { CoreNotificationActionClient(core: core) }
    public func processingHealth(stallAfterSeconds: UInt32 = 0, leaseGraceSeconds: UInt32 = 0) -> ProcessingHealthService {
        ProcessingHealthService(core: core, stallAfterSeconds: stallAfterSeconds, leaseGraceSeconds: leaseGraceSeconds)
    }
    public func shadowReview(configuration: ShadowReviewConfiguration = .off) -> ShadowReviewService {
        ShadowReviewService(core: core, configuration: configuration)
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
/// The root refuses a module whose `firstOperationID` is below `ServiceCompositionRoot.minimumModuleOperationID`, at or
/// above `JobRunnerService.firstOperationID` (the job runner claims every event from there up), or within `ServiceCompositionRoot.operationRangeWidth` of the range of the root's own clients or another module, so
/// two clients can never take each other's events. Modules are attached in the order given, after the job runner.
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
        readSession: ReadAuthenticationSession,
        notificationCenter: NotificationCenterProviding,
        permissionObservations: NotificationPermissionObservationStore = InMemoryNotificationPermissionObservationStore(),
        reachability: NetworkReachability,
        backgroundGrant: BackgroundExecutionGrant? = nil,
        lifecycleNotifications: NotificationCenter = NotificationCenter(),
        recognizer: OnDeviceRecognizing? = nil
    ) -> CompositionAdapters {
        CompositionAdapters(
            kind: .injectedForTests, sender: sender, notificationCenter: notificationCenter,
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
/// * **Extension seam.** `CompositionModule`s attach after the job runner without any edit here.
///
/// Build it on the main queue and call `shutDown()` before releasing it: that stops the job runner, ends waiting
/// requests, unregisters the native effects and closes the core.
public final class ServiceCompositionRoot {
    /// The smallest first operation identifier a module may use; below it lie the capture importer's identifiers.
    public static let minimumModuleOperationID: UInt64 = 1 << 32
    /// How far apart two clients' first operation identifiers must be.
    public static let operationRangeWidth: UInt64 = 1 << 40

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
    public func start(isActive: Bool) {
        stateLock.lock()
        guard !isShutDown, !hasStarted else {
            stateLock.unlock()
            return
        }
        hasStarted = true
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
            backgroundGrant: adapters.backgroundGrant)
        let permissionObserver = NotificationPermissionLifecycleObserver(
            reconciler: reminderReconciler, notificationCenter: center,
            onReport: { [weak self] report in self?.recordReminderReport(report) })
        self.jobObserver = jobObserver
        self.permissionObserver = permissionObserver
        let recovering = ingress
        stateLock.unlock()

        recovering?.recover { _ in }
        jobObserver.start(isActive: isActive)
        permissionObserver.start(isActive: isActive)
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
        let permissionReconciler: NotificationPermissionReconciler
        let foreground: ForegroundState
    }

    /// First operation identifiers of the root's own clients; the importer's identifiers start at 1.
    private static let ownOperationRanges: [(name: String, start: UInt64)] = [
        ("transcription", TranscriptionCoreClient.firstOperationID),
        ("reminders", CoreReminderReconciler.firstOperationID),
        ("item status", CoreItemStatusReader.firstOperationID),
        ("job runner", JobRunnerService.firstOperationID),
    ]

    private static func validate(modules: [CompositionModule]) throws {
        var identifiers = Set<String>()
        var claimed: [(name: String, start: UInt64)] = ownOperationRanges
        for module in modules {
            guard identifiers.insert(module.identifier).inserted else {
                throw CompositionError.duplicateModule(module.identifier)
            }
            let start = module.firstOperationID
            guard start >= minimumModuleOperationID, start < JobRunnerService.firstOperationID else {
                throw CompositionError.operationRangeConflict(module.identifier)
            }
            for other in claimed {
                let distance = start > other.start ? start - other.start : other.start - start
                if distance < operationRangeWidth {
                    throw CompositionError.operationRangeConflict(module.identifier)
                }
            }
            claimed.append((module.identifier, start))
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

            let context = CompositionContext(
                clients: CompositionCoreClients(core: core), privateReads: CompositionPrivateReads(gate: readGate),
                storageRootURL: layout.rootDirectory, sender: adapters.sender, now: environment.now)
            for module in modules {
                try module.attach(to: context)
                undo.append { module.shutDown() }
                router.register { module.handle($0) }
            }

            return Assembly(
                layout: layout, protectionFailureTotal: report.failures.count, core: core, ingress: ingress,
                statusReader: statusReader, jobRunner: jobRunner, transcription: transcription,
                reminderCore: reminderCore, permissionReconciler: permissionReconciler, foreground: foreground)
        } catch {
            for step in undo.reversed() {
                step()
            }
            throw error
        }
    }
}
