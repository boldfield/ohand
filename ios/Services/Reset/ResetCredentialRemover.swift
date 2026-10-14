import Foundation
import Security

/// What a Keychain lookup of one item answered: the status and, when the item exists, the identity of its secret.
struct ResetSecretLookup: Equatable {
    let status: OSStatus
    let identity: ResetSecretIdentity?
}

/// The Keychain calls reset needs that `CredentialService` deliberately does not expose: the metadata that identifies
/// which secret an item holds, and a removal conditioned on it. Production uses `SecurityResetSecretKeychain`; tests
/// inject a fake to produce exact `OSStatus` outcomes and to track identities beside a fake credential store.
protocol ResetSecretKeychain {
    /// The identity of the item's secret. `errSecItemNotFound` with no identity when there is no item.
    func identity(of key: KeychainItemKey) -> ResetSecretLookup
    /// Removes the item only while its secret still has `identity`, in one Keychain call: a secret written since is
    /// not matched and stays. `errSecItemNotFound` when no item with that identity exists.
    func remove(_ key: KeychainItemKey, identity: ResetSecretIdentity) -> OSStatus
}

/// The real data-protection Keychain. The identity is the item's creation and modification instants, which the Keychain
/// sets itself: `SecItemUpdate` moves the modification instant, and a delete followed by an add moves both. The removal
/// names those instants in its query, so the Keychain deletes the item only if they are still the same.
struct SecurityResetSecretKeychain: ResetSecretKeychain {
    private func identityQuery(for key: KeychainItemKey) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: key.service,
            kSecAttrAccount as String: key.account,
        ]
    }

    func identity(of key: KeychainItemKey) -> ResetSecretLookup {
        var query = identityQuery(for: key)
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess else { return ResetSecretLookup(status: status, identity: nil) }
        guard let attributes = result as? [String: Any],
            let createdAt = attributes[kSecAttrCreationDate as String] as? Date,
            let modifiedAt = attributes[kSecAttrModificationDate as String] as? Date
        else {
            // An item whose metadata cannot be read is not one reset can identify, so it is not one it may remove.
            return ResetSecretLookup(status: errSecDecode, identity: nil)
        }
        return ResetSecretLookup(status: status, identity: ResetSecretIdentity(createdAt: createdAt, modifiedAt: modifiedAt))
    }

    func remove(_ key: KeychainItemKey, identity: ResetSecretIdentity) -> OSStatus {
        var query = identityQuery(for: key)
        query[kSecAttrCreationDate as String] = identity.createdAt
        query[kSecAttrModificationDate as String] = identity.modifiedAt
        return SecItemDelete(query as CFDictionary)
    }
}

/// Removes, for a `remove` choice, exactly the secrets that were stored when the choice was made. It addresses the same
/// Keychain items as `CredentialService` (a generic-password item under the service name, with the validated reference
/// as the account) because that service, by design, lets nothing but references and statuses out, while reset must tell
/// the secret it was asked to remove from one the user stored since. Its errors are `CredentialError` values, which
/// carry no reference and no secret.
final class KeychainResetCredentialRemover: ResetCredentialRemoving {
    private let serviceName: String
    private let keychain: ResetSecretKeychain

    init(serviceName: String = CredentialService.defaultServiceName, keychain: ResetSecretKeychain = SecurityResetSecretKeychain()) {
        self.serviceName = serviceName
        self.keychain = keychain
    }

    func secretIdentity(reference: String) throws -> ResetSecretIdentity? {
        let lookup = keychain.identity(of: try itemKey(for: reference))
        switch lookup.status {
        case errSecSuccess:
            guard let identity = lookup.identity else { throw CredentialError.storage(.status, errSecDecode) }
            return identity
        case errSecItemNotFound:
            return nil
        default:
            throw Self.failure(for: lookup.status, operation: .status)
        }
    }

    func deleteCredential(reference: String, identity: ResetSecretIdentity) throws {
        let key = try itemKey(for: reference)
        // Nothing is removed blindly: the item must still hold the bound secret.
        guard try stillHolds(identity, key: key) else { return }
        let status = keychain.remove(key, identity: identity)
        switch status {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            // Removed or overwritten between the check and the removal, which is fine. An item that is still there with
            // the same identity means the Keychain did not condition the removal on it; that is a failure to report, not
            // a reason to remove whatever is stored now.
            guard try stillHolds(identity, key: key) else { return }
            throw CredentialError.storage(.delete, status)
        default:
            throw Self.failure(for: status, operation: .delete)
        }
    }

    private func stillHolds(_ identity: ResetSecretIdentity, key: KeychainItemKey) throws -> Bool {
        let lookup = keychain.identity(of: key)
        switch lookup.status {
        case errSecSuccess:
            return lookup.identity == identity
        case errSecItemNotFound:
            return false
        default:
            throw Self.failure(for: lookup.status, operation: .delete)
        }
    }

    private func itemKey(for reference: String) throws -> KeychainItemKey {
        guard let parsed = UUID(uuidString: reference) else { throw CredentialError.invalidReference }
        return KeychainItemKey(service: serviceName, account: parsed.uuidString)
    }

    private static func failure(for status: OSStatus, operation: CredentialOperation) -> CredentialError {
        if status == errSecInteractionNotAllowed { return .deviceLocked(operation) }
        return .storage(operation, status)
    }
}
