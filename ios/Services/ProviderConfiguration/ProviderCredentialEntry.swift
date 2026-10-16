import Foundation

/// What the root's `CredentialService` lets a settings flow do with a provider secret: store one, replace one, delete one
/// and ask whether one is still there. No call returns a secret. The only values that come back are an opaque reference
/// (which a provider profile holds), a `CredentialStatus`, or a `CredentialError` that never carries a reference or a
/// secret, so nothing here can leak one into a view, a log or a diagnostic.
protocol ProviderCredentialStore: AnyObject {
    func addCredential(_ secret: Data) throws -> String
    func updateCredential(_ secret: Data, reference: String) throws
    func deleteCredential(reference: String) throws
    func credentialStatus(reference: String) throws -> CredentialStatus
}

extension CredentialService: ProviderCredentialStore {}

/// Credential entry for provider setup, over the root's own `CredentialService`: the same Keychain items the native
/// provider transport resolves at dispatch.
///
/// Entering or removing a secret changes what can leave the device, so every call needs an authenticated read session,
/// checked when the call is made; a capture-only session gets `ProviderConfigurationError.notAuthenticated` and the
/// Keychain is not touched. Deleting a credential that a stored profile version still references leaves that version
/// unable to dispatch (its jobs wait for a credential); it changes no profile.
public final class ProviderCredentialEntry {
    private let store: ProviderCredentialStore
    private let session: ReadAuthenticationSession

    init(store: ProviderCredentialStore, session: ReadAuthenticationSession) {
        self.store = store
        self.session = session
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

    private func requireAuthentication() throws {
        guard session.currentAuthorization() != nil else { throw ProviderConfigurationError.notAuthenticated }
    }
}
