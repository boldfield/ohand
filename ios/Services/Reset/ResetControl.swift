import Foundation
import OhAndCoreBridge

/// What a reset call or a launch resume ended with. Fixed codes only: never identifiers, paths, content or error text.
public struct ResetOutcome: Equatable, Sendable {
    public let state: ResetState
    /// The core's latest progress, when the core was reached.
    public let progress: ResetProgress?
    public let blockers: [ResetBlocker]
}

/// Why a reset request or progress read was not carried out.
public enum ResetControlError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was asked of the core.
    case notAuthenticated
    /// The core could not answer a progress read.
    case core(CoreFailure)
}

/// The app's reset surface: the user's reset tap with their explicit credential choice, and a read of the core's
/// progress. Both need an authenticated read session when called. A reset that has started is not cancelled by a later
/// relock: the user confirmed it, and an interrupted one is finished at the next launch.
public final class ResetControl: @unchecked Sendable {
    private let service: ResetService
    private let core: ResetCore
    private let session: ReadAuthenticationSession

    init(service: ResetService, core: ResetCore, session: ReadAuthenticationSession) {
        self.service = service
        self.core = core
        self.session = session
    }

    /// Resets all captured content. Captures made while it runs are kept as fresh content and imported after the
    /// boundary is released. `pending` means a later call, or the next launch, finishes the rest.
    public func reset(credentialChoice: ResetCredentialChoice) async throws -> ResetOutcome {
        guard session.currentAuthorization() != nil else { throw ResetControlError.notAuthenticated }
        return Self.outcome(of: await service.reset(credentialChoice: credentialChoice))
    }

    /// The core's latest reset generation, or `nil` when no reset ever started.
    public func progress() async throws -> ResetProgress? {
        guard let authorization = session.currentAuthorization() else { throw ResetControlError.notAuthenticated }
        let latest: ResetProgress?
        do {
            latest = try await core.status()
        } catch let failure as CoreFailure {
            throw ResetControlError.core(failure)
        } catch {
            throw ResetControlError.core(CoreResetClient.unreadableAnswer)
        }
        // An answer that arrives after a relock is discarded unread.
        guard session.isCurrent(authorization) else { throw ResetControlError.notAuthenticated }
        return latest
    }

    /// Whether a reset that has started still owes native work (staged captures, cleanup, export temporaries, a
    /// credential choice), which the core's own progress does not show. Needs an authenticated read session.
    public func hasUnfinishedNativeWork() async throws -> Bool {
        guard session.currentAuthorization() != nil else { throw ResetControlError.notAuthenticated }
        return await service.hasUnfinishedNativeWork()
    }

    /// Finishes a reset that has not finished, with the credential choice the user made when starting it and nothing
    /// new: it starts no further reset. Needs an authenticated read session when called from the screen.
    public func finishPending() async throws -> ResetOutcome {
        guard session.currentAuthorization() != nil else { throw ResetControlError.notAuthenticated }
        return await resumeInterrupted()
    }

    /// Finishes an interrupted reset. Needs no session: it only completes what the user already confirmed.
    func resumeInterrupted() async -> ResetOutcome {
        Self.outcome(of: await service.resume())
    }

    private static func outcome(of report: ResetReport) -> ResetOutcome {
        ResetOutcome(state: report.state, progress: report.progress, blockers: report.blockers)
    }
}

/// The item cleanup `ResetService` runs, resolved when it is needed: the deletion cleanup service belongs to the
/// lifecycle coordinator, which is connected after the reset service is built. Until it is, the cleanup reports that it
/// could not list anything, so the reset stays pending and a later pass finishes it.
final class ResetItemCleanupBridge: ResetItemCleanup, @unchecked Sendable {
    private let lock = NSLock()
    private var provider: (@Sendable () -> DeletionCleanupService?)?

    func setProvider(_ provider: (@Sendable () -> DeletionCleanupService?)?) {
        lock.lock()
        self.provider = provider
        lock.unlock()
    }

    func reconcile() async -> DeletionReconcileReport {
        lock.lock()
        let service = provider?()
        lock.unlock()
        guard let service else {
            return DeletionReconcileReport(listingFailure: .coreUnavailable, items: [])
        }
        return await service.reconcile()
    }
}
