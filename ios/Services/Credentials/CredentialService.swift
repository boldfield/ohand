import Foundation
import Security

/// Errors that can occur during credential operations
public enum CredentialError: LocalizedError {
    case addFailed(String)
    case updateFailed(String)
    case deleteFailed(String)
    case statusError(String)
    case invalidReference(String)
    case keyNotFound
    case unexpectedStatus(OSStatus)

    public var errorDescription: String? {
        switch self {
        case .addFailed(let msg):
            return "Failed to add credential: \(msg)"
        case .updateFailed(let msg):
            return "Failed to update credential: \(msg)"
        case .deleteFailed(let msg):
            return "Failed to delete credential: \(msg)"
        case .statusError(let msg):
            return "Failed to query credential status: \(msg)"
        case .invalidReference(let msg):
            return "Invalid credential reference: \(msg)"
        case .keyNotFound:
            return "Credential key not found in Keychain"
        case .unexpectedStatus(let status):
            return "Unexpected Keychain error code: \(status)"
        }
    }
}

/// Status of a credential in storage
public enum CredentialStatus: Equatable {
    case present
    case absent
    case invalidated
}

/// Manages provider secrets in native Keychain behind opaque references.
/// Secrets never leave the Keychain and are resolved only at dispatch time by the HTTP transport.
public class CredentialService {
    private let keychainService: String
    private let keychain: KeychainBoundary

    /// Creates a credential service with the specified Keychain service name
    public init(keychainService: String = "com.boldfield.ohand.credentials", keychain: KeychainBoundary? = nil) {
        self.keychainService = keychainService
        self.keychain = keychain ?? RealKeychain()
    }

    /// Adds a new credential and returns an opaque reference
    /// - Parameter secret: The secret value to store (never logged or exported)
    /// - Returns: An opaque credential reference for use in provider profiles
    /// - Throws: CredentialError if storage fails
    public func addCredential(_ secret: Data) throws -> String {
        let reference = UUID().uuidString
        try updateCredential(secret, reference: reference)
        return reference
    }

    /// Updates an existing credential by reference
    /// - Parameters:
    ///   - secret: The new secret value to store
    ///   - reference: The credential reference returned from add or a prior update
    /// - Throws: CredentialError if storage fails
    public func updateCredential(_ secret: Data, reference: String) throws {
        guard !reference.isEmpty else {
            throw CredentialError.invalidReference("Empty credential reference")
        }

        let query = keychainQueryAttributes(for: reference)
        let status = keychain.update(query as CFDictionary, [kSecValueData as String: secret] as CFDictionary)

        switch status {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            try addKeychainItem(secret, reference: reference)
        case errSecUserCanceled:
            throw CredentialError.updateFailed("User cancelled Keychain access")
        case errSecInteractionNotAllowed:
            throw CredentialError.updateFailed("Keychain interaction not allowed (device may be locked)")
        case errSecAuthFailed:
            throw CredentialError.updateFailed("Keychain authentication failed")
        default:
            throw CredentialError.unexpectedStatus(status)
        }
    }

    /// Deletes a credential by reference
    /// - Parameter reference: The credential reference to delete
    /// - Throws: CredentialError if deletion fails
    public func deleteCredential(reference: String) throws {
        guard !reference.isEmpty else {
            throw CredentialError.invalidReference("Empty credential reference")
        }

        let query = keychainQueryAttributes(for: reference)
        let status = keychain.delete(query as CFDictionary)

        switch status {
        case errSecSuccess, errSecItemNotFound:
            return
        case errSecUserCanceled:
            throw CredentialError.deleteFailed("User cancelled Keychain access")
        case errSecInteractionNotAllowed:
            throw CredentialError.deleteFailed("Keychain interaction not allowed (device may be locked)")
        case errSecAuthFailed:
            throw CredentialError.deleteFailed("Keychain authentication failed")
        default:
            throw CredentialError.unexpectedStatus(status)
        }
    }

    /// Gets the current status of a credential without retrieving the secret
    /// - Parameter reference: The credential reference to check
    /// - Returns: CredentialStatus indicating presence, absence, or invalidation
    /// - Throws: CredentialError if status check fails
    public func credentialStatus(reference: String) throws -> CredentialStatus {
        guard !reference.isEmpty else {
            throw CredentialError.invalidReference("Empty credential reference")
        }

        var query = keychainQueryAttributes(for: reference)
        query[kSecReturnData as String] = true

        var result: CFTypeRef?
        let status = keychain.copyMatching(query as CFDictionary, &result)

        switch status {
        case errSecSuccess:
            if let data = result as? Data, data.isEmpty {
                return .invalidated
            }
            return .present
        case errSecItemNotFound:
            return .absent
        case errSecUserCanceled:
            throw CredentialError.statusError("User cancelled Keychain access")
        case errSecInteractionNotAllowed:
            throw CredentialError.statusError("Keychain interaction not allowed (device may be locked)")
        case errSecAuthFailed:
            throw CredentialError.statusError("Keychain authentication failed")
        default:
            throw CredentialError.unexpectedStatus(status)
        }
    }

    /// Retrieves a credential by reference (used only by HTTP transport at dispatch time)
    /// Internal use only - never expose this to webview, logs, or exports
    internal func retrieveCredential(reference: String) throws -> Data {
        guard !reference.isEmpty else {
            throw CredentialError.invalidReference("Empty credential reference")
        }

        var query = keychainQueryAttributes(for: reference)
        query[kSecReturnData as String] = true

        var result: CFTypeRef?
        let status = keychain.copyMatching(query as CFDictionary, &result)

        switch status {
        case errSecSuccess:
            if let secret = result as? Data {
                return secret
            }
            throw CredentialError.keyNotFound
        case errSecItemNotFound:
            throw CredentialError.keyNotFound
        case errSecUserCanceled:
            throw CredentialError.statusError("User cancelled Keychain access")
        case errSecInteractionNotAllowed:
            throw CredentialError.statusError("Keychain interaction not allowed (device may be locked)")
        case errSecAuthFailed:
            throw CredentialError.statusError("Keychain authentication failed")
        default:
            throw CredentialError.unexpectedStatus(status)
        }
    }

    // MARK: - Private

    private func keychainQueryAttributes(for reference: String) -> [String: Any] {
        return [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: reference
        ]
    }

    private func keychainAddAttributes(for reference: String) -> [String: Any] {
        var attributes = keychainQueryAttributes(for: reference)
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return attributes
    }

    private func addKeychainItem(_ secret: Data, reference: String) throws {
        var attributes = keychainAddAttributes(for: reference)
        attributes[kSecValueData as String] = secret

        let status = keychain.add(attributes as CFDictionary, nil)

        switch status {
        case errSecSuccess:
            return
        case errSecDuplicateItem:
            let query = keychainQueryAttributes(for: reference)
            let updateStatus = keychain.update(query as CFDictionary, [kSecValueData as String: secret] as CFDictionary)
            switch updateStatus {
            case errSecSuccess:
                return
            case errSecItemNotFound:
                throw CredentialError.addFailed("Item not found during recovery from duplicate")
            case errSecUserCanceled:
                throw CredentialError.addFailed("User cancelled Keychain access")
            case errSecInteractionNotAllowed:
                throw CredentialError.addFailed("Keychain interaction not allowed (device may be locked)")
            case errSecAuthFailed:
                throw CredentialError.addFailed("Keychain authentication failed")
            default:
                throw CredentialError.unexpectedStatus(updateStatus)
            }
        case errSecUserCanceled:
            throw CredentialError.addFailed("User cancelled Keychain access")
        case errSecInteractionNotAllowed:
            throw CredentialError.addFailed("Keychain interaction not allowed (device may be locked)")
        case errSecAuthFailed:
            throw CredentialError.addFailed("Keychain authentication failed")
        default:
            throw CredentialError.unexpectedStatus(status)
        }
    }
}
