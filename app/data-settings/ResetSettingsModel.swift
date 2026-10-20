import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The reset operations this section uses. `ResetControl` is the production implementation; the section reuses its
/// operations and adds none.
protocol ResetControlling: AnyObject {
    func reset(credentialChoice: ResetCredentialChoice) async throws -> ResetOutcome
    func progress() async throws -> ResetProgress?
    /// Whether native work (staged captures, cleanup, temporaries, a credential choice) is still owed.
    func hasUnfinishedNativeWork() async throws -> Bool
    /// Finishes a reset that has not finished, under the choice made when it started; starts no further reset.
    func finishPending() async throws -> ResetOutcome
}

extension ResetControl: ResetControlling {}

/// What the reset section is showing.
enum ResetSettingsPhase: Equatable {
    case idle
    /// The credential choice is made and the user has not yet confirmed.
    case confirming
    case resetting
    /// The core or a native step has not finished; the text says what is outstanding.
    case pending(String)
    case completed
    case failed(String)
    case locked
}

/// The state of the reset ("delete all data") section.
///
/// Rules this type keeps:
/// * A reset starts only after an explicit credential choice (keep or remove) and a separate confirmation. Neither has
///   a default.
/// * The reset is `completed` only when the service reports the core completed the generation and every native step
///   converged. Anything else stays `pending` with what is outstanding, and the user can retry; opening the section
///   again reads the core's progress and the native markers, so an interrupted reset is shown as pending, not forgotten.
/// * A retry finishes the pending reset under the credential choice made when it started. It never takes a new choice
///   and never starts another reset, so a pending `remove` cannot be turned into `keep` (or the reverse) by retrying.
/// * A reset that has started is not cancelled by leaving the screen or by a relock. Its outcome holds counts and fixed
///   words only, so it may be shown after a relock.
/// * The section never claims that exported or backed-up copies were removed.
@MainActor
final class ResetSettingsModel: ObservableObject {
    @Published private(set) var phase: ResetSettingsPhase = .idle
    @Published var credentialChoice: ResetCredentialChoice?

    private let controls: ResetControlling
    private let readScope: () -> CompositionReadScope

    nonisolated init(controls: ResetControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isBusy: Bool { phase == .resetting }

    /// Whether the user can go on to the confirmation step.
    var canContinue: Bool { credentialChoice != nil && (phase == .idle || isFailed) }

    var canRetry: Bool { phase.isPending }

    private var isFailed: Bool {
        if case .failed = phase { return true }
        return false
    }

    func beginConfirmation() {
        guard readScope() == .authenticated else { lock(); return }
        guard canContinue else { return }
        phase = .confirming
    }

    func cancelConfirmation() {
        if phase == .confirming { phase = .idle }
    }

    /// Starts the reset with the chosen credential option.
    func confirmReset() async {
        guard phase == .confirming else { return }
        await run()
    }

    /// Asks the service to finish a pending reset under the choice it was started with.
    func retry() async {
        guard canRetry else { return }
        guard readScope() == .authenticated else { lock(); return }
        let pending = phase
        phase = .resetting
        do {
            apply(try await controls.finishPending())
        } catch ResetControlError.notAuthenticated {
            // A relock keeps the pending reset's progress visible, as `lock()` does; it holds no private content.
            phase = pending
        } catch {
            phase = .failed(DeletionText.unavailableResetText)
        }
    }

    private func run() async {
        guard let choice = credentialChoice else { return }
        guard readScope() == .authenticated else { lock(); return }
        phase = .resetting
        do {
            apply(try await controls.reset(credentialChoice: choice))
        } catch ResetControlError.notAuthenticated {
            phase = .locked
        } catch {
            phase = .failed(DeletionText.unavailableResetText)
        }
    }

    private func apply(_ outcome: ResetOutcome) {
        switch outcome.state {
        case .completed:
            phase = .completed
        case .pending:
            phase = .pending(DeletionText.pendingDetail(blockers: outcome.blockers, progress: outcome.progress))
        case .notStarted:
            phase = .failed(DeletionText.notStarted(blockers: outcome.blockers))
        }
    }

    /// Reads the core's progress and the native markers, so a reset a previous run left unfinished is shown as pending.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .idle || phase == .locked || phase.isPending else { return }
        do {
            let progress = try await controls.progress()
            let nativeOwed = try await controls.hasUnfinishedNativeWork()
            guard readScope() == .authenticated, phase == .idle || phase == .locked || phase.isPending else { return }
            if progress?.phase == .pending {
                phase = .pending(DeletionText.pendingDetail(blockers: [], progress: progress))
            } else if nativeOwed {
                phase = .pending(DeletionText.nativeWorkOwedText)
            } else if phase == .locked {
                phase = .idle
            }
        } catch ResetControlError.notAuthenticated {
            lock()
        } catch {
            // An earlier reset may be unfinished, so an unreadable state is never shown as a fresh start. Retry only
            // finishes a pending reset; it starts none.
            guard readScope() == .authenticated, phase == .idle || phase.isPending else { return }
            phase = .pending(DeletionText.progressUnreadableResetText)
        }
    }

    /// Call when the screen goes away. Only the transient choice and a stale error are dropped.
    func clear() {
        if phase == .confirming || isFailed { phase = .idle }
        if phase == .idle { credentialChoice = nil }
    }

    private func lock() {
        if phase == .resetting || phase.isPending || phase == .completed { return }
        phase = .locked
    }
}

extension ResetSettingsPhase {
    var isPending: Bool {
        if case .pending = self { return true }
        return false
    }
}
