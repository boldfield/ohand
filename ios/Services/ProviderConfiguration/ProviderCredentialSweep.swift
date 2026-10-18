import Foundation
import OhAndCoreBridge

/// What the credential sweep needs from the Keychain side: the stored references, the in-process leases and deletion.
/// `CredentialService` is the production implementation. No call returns a secret.
protocol ProviderCredentialSweepStore: AnyObject {
    func listReferences() throws -> Set<String>
    func leasedReferences() -> Set<String>
    func deleteCredential(reference: String) throws
}

extension CredentialService: ProviderCredentialSweepStore {}

/// What the credential sweep needs from the core.
protocol ProviderCredentialSweepCore: Sendable {
    /// Whether a reset generation is pending. Throws when that cannot be read.
    func isResetPending() async throws -> Bool
    /// Every reference any stored profile version holds, revoked versions included. Throws when it cannot be read.
    func heldReferences() async throws -> Set<String>
}

/// What one sweep did. Counts only: never a reference.
public enum ProviderCredentialSweepOutcome: Equatable, Sendable {
    /// Every unowned item was looked at; `deleted` were removed and `failed` could not be and wait for the next sweep.
    case swept(deleted: Int, failed: Int)
    /// A reset is pending, so nothing was looked at.
    case skippedForReset
    /// The Keychain, the core or the reset state could not be read, so nothing was deleted.
    case skippedUnreadable
    /// The read session was not authenticated when the sweep started or before it deleted, so nothing was deleted.
    case skippedLocked
}

/// Removes provider secrets that nothing owns: items under the credential service that no stored profile version
/// holds and no live operation in this process has leased. A secret stored for a draft that never saved, because the
/// process ended or a best-effort deletion failed, is removed here on a later visit to provider settings.
///
/// One pass reads, in this order:
/// 1. the references stored in the Keychain, K;
/// 2. the references leased in this process, L;
/// 3. every reference a stored profile version holds, revoked ones included, P.
/// It then deletes K − L − P. The order is what makes it safe without a second record of ownership: an item added
/// after step 1 is not in K, and a save that commits and then releases its lease is either still leased at step 2 or
/// committed before step 3 and so in P.
///
/// The pass is skipped while a reset is pending, and deletes nothing if any read fails. A reset's credential claims
/// are not read separately: a claimed reference is one the reset is itself deleting, its bound versions stay stored
/// (revoked) and so in P, and the reset's deletion treats an item already gone as done.
public final class ProviderCredentialSweep: @unchecked Sendable {
    private let store: ProviderCredentialSweepStore
    private let core: ProviderCredentialSweepCore
    private let session: ReadAuthenticationSession

    init(store: ProviderCredentialSweepStore, core: ProviderCredentialSweepCore, session: ReadAuthenticationSession) {
        self.store = store
        self.core = core
        self.session = session
    }

    /// Runs one pass. Needs an authenticated read session when it starts and again before it deletes anything.
    public func run() async -> ProviderCredentialSweepOutcome {
        guard let authorization = session.currentAuthorization() else { return .skippedLocked }
        do {
            if try await core.isResetPending() { return .skippedForReset }
        } catch {
            return .skippedUnreadable
        }
        let stored: Set<String>
        let leased: Set<String>
        let held: Set<String>
        do {
            stored = try store.listReferences()
            leased = store.leasedReferences()
            held = Set(try await core.heldReferences().compactMap { UUID(uuidString: $0)?.uuidString })
        } catch {
            return .skippedUnreadable
        }
        guard session.isCurrent(authorization) else { return .skippedLocked }
        var deleted = 0
        var failed = 0
        for reference in stored.subtracting(leased).subtracting(held).sorted() {
            do {
                try store.deleteCredential(reference: reference)
                deleted += 1
            } catch {
                failed += 1
            }
        }
        return .swept(deleted: deleted, failed: failed)
    }
}

/// The production core side of the sweep: the reset state from `ResetControl`, and the reference of every stored
/// profile version from the provider-profile client, the same core read `ProviderCredentialEntry` uses by version.
struct CoreProviderCredentialSweepSource: ProviderCredentialSweepCore {
    let profiles: ProviderProfileCore?
    let reset: ResetControl

    func isResetPending() async throws -> Bool {
        try await reset.progress()?.phase == .pending
    }

    func heldReferences() async throws -> Set<String> {
        guard let profiles else { throw ProviderConfigurationError.unreadable }
        guard case .listing(let listing) = try await Self.ask(profiles, .list) else {
            throw ProviderConfigurationError.unreadable
        }
        var held: Set<String> = []
        for version in listing.profiles.flatMap(\.versions) where version.credentialBound {
            let answer = try await Self.ask(profiles, .credentialReference(profileVersion: version.profileVersion))
            guard case .credentialReference(let bound) = answer else { throw ProviderConfigurationError.unreadable }
            if let reference = bound.credentialReference, !reference.isEmpty { held.insert(reference) }
        }
        return held
    }

    private static func ask(
        _ profiles: ProviderProfileCore, _ command: ProviderProfileCommand
    ) async throws -> ProviderProfileAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ProviderProfileAnswer, CoreFailure>, Never>) in
            profiles.submit(command) { continuation.resume(returning: $0) }
        }
        return try result.get()
    }
}

extension ServiceCompositionRoot {
    /// The sweep of unowned provider secrets over this root's Keychain items, stored profiles and reset state. The
    /// provider settings screen runs it when it loads, before it accepts a save.
    public var providerCredentialSweep: ProviderCredentialSweep {
        ProviderCredentialSweep(
            store: adapters.credentials,
            core: CoreProviderCredentialSweepSource(profiles: providerCredentials.profiles, reset: resetControl),
            session: readSession)
    }
}
