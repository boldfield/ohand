import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The retention operations this screen uses. `RetentionControlService` is the production implementation; the screen
/// reuses its operations and adds none.
protocol RetentionControlling: AnyObject {
    func status() async throws -> RetentionStatus
    func setPeriod(seconds: Int64) async throws -> RetentionPolicySetting
    func retryTranscription(itemID: String) async throws -> String
    func deleteAudio(itemID: String) async throws -> AudioDeletionReceipt
}

extension RetentionControlService: RetentionControlling {}

/// What the retention screen is showing.
enum RetentionSettingsPhase: Equatable {
    case idle
    case loading
    case loaded
    /// The read session is not authenticated, so no stored state is shown.
    case locked
    case failed(String)
}

/// What the last action on one unresolved recording reported.
enum RetentionActionState: Equatable {
    case working
    case queued
    case removed
    case failed(String)

    var message: String? {
        switch self {
        case .working: return "Working…"
        case .queued: return "Queued. The recording will be turned into text; this list updates when it is done."
        case .removed: return "The audio was removed. The item and its text are kept."
        case .failed(let message): return message
        }
    }
}

/// The state of the retention screen: the stored period, the storage pressure and the recordings that are an item's only
/// source.
///
/// Rules this type keeps:
/// * Stored state appears only while the read session is authenticated, checked before a read is sent and again when
///   its answer arrives; a relock empties the screen.
/// * The period shown is the core's stored value, replaced only by the core's own answer. A refused or failed change
///   leaves the shown period unchanged and says so.
/// * Nothing is reported as done unless the operation succeeded: a failed retry or removal shows its error and the
///   recording stays listed.
/// * Every operation is a separate request. None of it waits on, or is awaited by, the capture path, so capture stays
///   available while a sweep or an action runs.
@MainActor
final class RetentionSettingsModel: ObservableObject {
    @Published private(set) var phase: RetentionSettingsPhase = .idle
    @Published private(set) var status: RetentionStatus?
    @Published private(set) var periodState: RetentionActionState?
    @Published private(set) var actions: [String: RetentionActionState] = [:]

    /// The period the user is choosing, in whole days. Applied by `applyPeriod()`.
    @Published var chosenDays: Int = 7

    static let minimumDays = 1
    static let maximumDays = Int(RetentionPolicySetting.maximumPeriodSeconds / 86_400)

    private let controls: RetentionControlling
    private let readScope: () -> CompositionReadScope
    private var generation = 0
    private var lastStoredDays = 7

    nonisolated init(controls: RetentionControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isApplyingPeriod: Bool { periodState == .working }

    /// Reads the policy, pressure and unresolved recordings again.
    func refresh() async {
        guard readScope() == .authenticated else {
            lock()
            return
        }
        generation += 1
        let thisGeneration = generation
        if status == nil { phase = .loading }
        do {
            let fresh = try await controls.status()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            status = fresh
            let storedDays = max(Self.minimumDays, min(Self.maximumDays, Int(fresh.policy.periodSeconds / 86_400)))
            // An unsaved stepper value survives a refresh; only an untouched one follows the stored period.
            if status == nil || chosenDays == lastStoredDays { chosenDays = storedDays }
            lastStoredDays = storedDays
            actions = actions.filter { entry in fresh.unresolved.contains { $0.itemID == entry.key } }
            phase = .loaded
        } catch {
            guard thisGeneration == generation else { return }
            if case RetentionControlError.notAuthenticated = error {
                lock()
            } else {
                phase = .failed(RetentionSettingsText.failure(error))
            }
        }
    }

    /// Stores `chosenDays` as the retention period.
    func applyPeriod() async {
        guard readScope() == .authenticated else { lock(); return }
        guard !isApplyingPeriod else { return }
        let days = chosenDays
        guard days >= Self.minimumDays, days <= Self.maximumDays else {
            periodState = .failed(RetentionSettingsText.failure(RetentionControlError.invalidRequest))
            return
        }
        periodState = .working
        do {
            let stored = try await controls.setPeriod(seconds: Int64(days) * 86_400)
            guard readScope() == .authenticated else { lock(); return }
            if let current = status {
                status = RetentionStatus(
                    policy: stored, storagePressure: current.storagePressure, unresolved: current.unresolved)
            }
            periodState = nil
            await refresh()
        } catch {
            if case RetentionControlError.notAuthenticated = error { lock(); return }
            periodState = .failed(RetentionSettingsText.failure(error))
        }
    }

    func retryTranscription(itemID: String) async {
        await act(itemID: itemID, success: .queued) { [controls] in
            _ = try await controls.retryTranscription(itemID: itemID)
        }
    }

    func deleteAudio(itemID: String) async {
        await act(itemID: itemID, success: .removed) { [controls] in
            _ = try await controls.deleteAudio(itemID: itemID)
        }
    }

    /// Call when the screen goes away or the app locks: drops every stored detail and ignores answers still pending.
    func clear() {
        generation += 1
        status = nil
        actions = [:]
        periodState = nil
        phase = .idle
    }

    private func act(
        itemID: String, success: RetentionActionState, operation: () async throws -> Void
    ) async {
        guard readScope() == .authenticated else { lock(); return }
        if actions[itemID] == .working { return }
        actions[itemID] = .working
        do {
            try await operation()
            guard readScope() == .authenticated else { lock(); return }
            actions[itemID] = success
        } catch {
            if case RetentionControlError.notAuthenticated = error { lock(); return }
            actions[itemID] = .failed(RetentionSettingsText.failure(error))
        }
        await refresh()
    }

    private func lock() {
        clear()
        phase = .locked
    }
}
