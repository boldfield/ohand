import Foundation
import OhAndCoreBridge

/// What the root's `CredentialService` lets a settings flow do with a provider secret: store one, replace one, delete one
/// and ask whether one is still there. No call returns a secret. The only values that come back are an opaque reference
/// (which a provider profile holds), a `CredentialStatus`, or a `CredentialError` that never carries a reference or a
/// secret, so nothing here can leak one into a view, a log or a diagnostic.
protocol ProviderCredentialStore: AnyObject {
    func addCredential(_ secret: Data) throws -> String
    func updateCredential(_ secret: Data, reference: String) throws
    func deleteCredential(reference: String) throws
    func credentialStatus(reference: String) throws -> CredentialStatus
    func releaseLease(reference: String)
}

extension CredentialService: ProviderCredentialStore {}

/// Credential entry for provider setup, over the root's own `CredentialService`: the same Keychain items the native
/// provider transport resolves at dispatch.
///
/// Entering or removing a secret changes what can leave the device, so every call needs an authenticated read session,
/// checked when the call is made; a capture-only session gets `ProviderConfigurationError.notAuthenticated` and the
/// Keychain is not touched. Deleting a credential that a stored profile version still references leaves that version
/// unable to dispatch (its jobs wait for a credential); it changes no profile.
///
/// A saved profile can also be repaired without its caller ever holding the reference: `replaceCredential`,
/// `credentialStatus` and `removeCredential` take a stored profile version, resolve its reference inside the services
/// layer (one core read that the listing never offers) and call the same `CredentialService` operations. Replacing
/// writes in place under the same reference, so the profile, its version and the jobs pinned to it are unchanged and
/// become dispatchable again. **Removing is by reference, not by version:** other stored versions that name the same
/// reference lose their secret too and report `credentialMissing`. An unknown version (`core` with `profile_not_found`)
/// and a version with no credential bound (`core` with `credential_not_bound`) throw distinct errors, and no answer or
/// error carries a reference or a secret.
public final class ProviderCredentialEntry: @unchecked Sendable {
    private let store: ProviderCredentialStore
    private let session: ReadAuthenticationSession
    /// Also read by the credential sweep, which lists what every stored version holds.
    let profiles: ProviderProfileCore?

    /// The `ProviderConfigurationError.core` code for a version that has no credential bound. An unknown version is the
    /// core's own `profile_not_found`.
    static let noCredentialBoundCode = "credential_not_bound"

    /// `profiles` serves the by-version operations; without it they throw `ProviderConfigurationError.unreadable`.
    init(store: ProviderCredentialStore, session: ReadAuthenticationSession, profiles: ProviderProfileCore? = nil) {
        self.store = store
        self.session = session
        self.profiles = profiles
    }

    /// Stores `secret` and returns the opaque reference to name in a `ProviderProfileDraft`.
    public func addCredential(_ secret: Data) throws -> String {
        try requireAuthentication()
        return try store.addCredential(secret)
    }

    /// Replaces the secret behind `reference`, re-binding the reference if its item is missing.
    public func updateCredential(_ secret: Data, reference: String) throws {
        try requireAuthentication()
        try store.updateCredential(secret, reference: reference)
    }

    /// Removes the secret behind `reference`. Deleting an absent credential succeeds.
    public func deleteCredential(reference: String) throws {
        try requireAuthentication()
        try store.deleteCredential(reference: reference)
    }

    /// Whether the secret behind `reference` is present, absent or unusable. Never reads the secret.
    public func credentialStatus(reference: String) throws -> CredentialStatus {
        try requireAuthentication()
        return try store.credentialStatus(reference: reference)
    }

    /// Ends the in-process lease that keeps a newly stored secret from the credential sweep. Call it once the core has
    /// committed a profile version that holds `reference`, or when nothing will save it and its deletion failed, so a
    /// later sweep can remove it. Needs no session: it touches no secret and widens nothing that can leave the device.
    public func releaseLease(reference: String) {
        store.releaseLease(reference: reference)
    }

    // MARK: by profile version

    /// Replaces the secret bound to the stored profile version, in place under its existing reference.
    public func replaceCredential(_ secret: Data, profileVersion: String) async throws {
        let reference = try await resolveReference(profileVersion: profileVersion)
        try store.updateCredential(secret, reference: reference)
        // A stored version holds the reference, so the item needs no lease even if the write had to re-create it.
        store.releaseLease(reference: reference)
    }

    /// Whether the secret bound to the stored profile version is present, absent or unusable. Never reads the secret.
    public func credentialStatus(profileVersion: String) async throws -> CredentialStatus {
        let reference = try await resolveReference(profileVersion: profileVersion)
        return try store.credentialStatus(reference: reference)
    }

    /// Removes the secret bound to the stored profile version, leaving it `credentialMissing`. Every stored version
    /// that names the same reference loses its secret too.
    public func removeCredential(profileVersion: String) async throws {
        let reference = try await resolveReference(profileVersion: profileVersion)
        try store.deleteCredential(reference: reference)
    }

    /// Authenticates, asks the core for the version's reference and authenticates again before the caller touches the
    /// Keychain, so a session that relocked during the read gets nothing.
    private func resolveReference(profileVersion: String) async throws -> String {
        try requireAuthentication()
        guard !profileVersion.isEmpty, profileVersion.utf8.count <= 512 else {
            throw ProviderConfigurationError.invalidRequest
        }
        guard let profiles else { throw ProviderConfigurationError.unreadable }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ProviderProfileAnswer, CoreFailure>, Never>) in
            profiles.submit(.credentialReference(profileVersion: profileVersion)) { continuation.resume(returning: $0) }
        }
        try requireAuthentication()
        switch result {
        case .success(.credentialReference(let answer)):
            guard let reference = answer.credentialReference, !reference.isEmpty else {
                throw ProviderConfigurationError.core(
                    CoreFailure(
                        errorClass: .permanent, code: Self.noCredentialBoundCode,
                        message: "the stored profile version has no credential bound"))
            }
            return reference
        case .success:
            throw ProviderConfigurationError.unreadable
        case .failure(let failure):
            switch failure.code {
            case "invalid_request": throw ProviderConfigurationError.invalidRequest
            case "read_not_authenticated", "read_relocked": throw ProviderConfigurationError.notAuthenticated
            default: throw ProviderConfigurationError.core(failure)
            }
        }
    }

    private func requireAuthentication() throws {
        guard session.currentAuthorization() != nil else { throw ProviderConfigurationError.notAuthenticated }
    }
}
