import Foundation
import Security

public enum CredentialOperation: String, Equatable {
    case add
    case update
    case delete
    case status
    case resolve
}

/// Health of the secret behind a credential reference. Safe to show in UI and pass to the core.
public enum CredentialStatus: String, Codable, Equatable {
    case present
    case absent
    case invalidated
}

/// Failure classes for credential operations. Cases carry only the operation and numeric `OSStatus`, never a
/// reference, service name or secret, so any rendering of an error is safe to log or show.
public enum CredentialError: Error, Equatable, CustomStringConvertible, LocalizedError {
    case invalidReference
    case emptySecret
    /// No item exists behind a reference that a dispatch tried to resolve.
    case notFound
    /// An item exists but cannot yield a usable secret (empty or undecodable). Not the same as locked.
    case invalidated
    /// The device is locked and the item is not readable yet. Transient; the secret is intact.
    case deviceLocked(CredentialOperation)
    case storage(CredentialOperation, OSStatus)

    public var description: String {
        switch self {
        case .invalidReference:
            return "credential reference is not valid"
        case .emptySecret:
            return "credential secret is empty"
        case .notFound:
            return "credential not found"
        case .invalidated:
            return "credential is invalidated"
        case .deviceLocked(let operation):
            return "credential \(operation.rawValue) unavailable while the device is locked"
        case .storage(let operation, let status):
            return "credential \(operation.rawValue) failed with Keychain status \(status)"
        }
    }

    public var errorDescription: String? { description }
}

/// Stores provider secrets in the native Keychain behind opaque references.
///
/// The only values that leave this type are references (`String`), `CredentialStatus` and `CredentialError`.
/// Secret bytes come back only through `resolveSecret`, which is module-internal so that the native provider
/// transport (same module) can attach a secret at dispatch while the app, control extensions, webview and
/// core bridge cannot call it. This type never logs.
public final class CredentialService {
    public static let defaultServiceName = "com.boldfield.ohand.credentials"

    private let serviceName: String
    private let keychain: KeychainBoundary

    public convenience init(serviceName: String = CredentialService.defaultServiceName) {
        self.init(serviceName: serviceName, keychain: SecurityKeychain())
    }

    init(serviceName: String, keychain: KeychainBoundary) {
        self.serviceName = serviceName
        self.keychain = keychain
    }

    /// Stores a new secret and returns the opaque reference that provider profiles hold.
    public func addCredential(_ secret: Data) throws -> String {
        guard !secret.isEmpty else { throw CredentialError.emptySecret }
        let reference = UUID().uuidString
        let status = keychain.insert(
            key: itemKey(for: reference), secret: secret, accessibility: .afterFirstUnlockThisDeviceOnly)
        try requireSuccess(status, operation: .add)
        return reference
    }

    /// Overwrites the secret behind a stable reference. If the item is missing (for example after a restore),
    /// the same reference is re-bound to the new secret.
    public func updateCredential(_ secret: Data, reference: String) throws {
        guard !secret.isEmpty else { throw CredentialError.emptySecret }
        let key = try validatedItemKey(for: reference)
        let accessibility = KeychainAccessibility.afterFirstUnlockThisDeviceOnly

        var status = keychain.replace(key: key, secret: secret, accessibility: accessibility)
        if status == errSecItemNotFound {
            status = keychain.insert(key: key, secret: secret, accessibility: accessibility)
            if status == errSecDuplicateItem {
                // Another writer created the item between our replace and insert; retry the replace once.
                status = keychain.replace(key: key, secret: secret, accessibility: accessibility)
            }
        }
        try requireSuccess(status, operation: .update)
    }

    /// Removes the secret behind a reference. Deleting an absent credential succeeds.
    public func deleteCredential(reference: String) throws {
        let key = try validatedItemKey(for: reference)
        let status = keychain.remove(key: key)
        if status == errSecItemNotFound { return }
        try requireSuccess(status, operation: .delete)
    }

    /// Reports the health of the item behind a reference from its metadata only. Secret bytes are never read on
    /// this path (`KeychainBoundary.inspect` returns just a status), so a health query cannot materialize them.
    /// An item whose value is empty can only come from outside this service (add and update reject empty
    /// secrets); it reports `present` here and fails explicitly with `invalidated` in `resolveSecret`.
    public func credentialStatus(reference: String) throws -> CredentialStatus {
        let key = try validatedItemKey(for: reference)
        let status = keychain.inspect(key: key)
        switch status {
        case errSecSuccess:
            return .present
        case errSecItemNotFound:
            return .absent
        case errSecDecode, errSecAuthFailed:
            return .invalidated
        default:
            throw Self.failure(for: status, operation: .status)
        }
    }

    /// Returns the secret for outbound dispatch. Module-internal on purpose: see the type documentation.
    func resolveSecret(reference: String) throws -> Data {
        let key = try validatedItemKey(for: reference)
        let result = keychain.read(key: key)
        switch result.status {
        case errSecSuccess:
            guard Self.hasUsableSecret(result.secret), let secret = result.secret else {
                throw CredentialError.invalidated
            }
            return secret
        case errSecItemNotFound:
            throw CredentialError.notFound
        case errSecDecode, errSecAuthFailed:
            throw CredentialError.invalidated
        default:
            throw Self.failure(for: result.status, operation: .resolve)
        }
    }

    private func itemKey(for reference: String) -> KeychainItemKey {
        KeychainItemKey(service: serviceName, account: reference)
    }

    private func validatedItemKey(for reference: String) throws -> KeychainItemKey {
        guard let parsed = UUID(uuidString: reference) else { throw CredentialError.invalidReference }
        return itemKey(for: parsed.uuidString)
    }

    private func requireSuccess(_ status: OSStatus, operation: CredentialOperation) throws {
        guard status == errSecSuccess else { throw Self.failure(for: status, operation: operation) }
    }

    private static func hasUsableSecret(_ secret: Data?) -> Bool {
        guard let secret else { return false }
        return !secret.isEmpty
    }

    private static func failure(for status: OSStatus, operation: CredentialOperation) -> CredentialError {
        if status == errSecInteractionNotAllowed { return .deviceLocked(operation) }
        return .storage(operation, status)
    }
}
