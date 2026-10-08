import Foundation
import Security

/// Accessibility class applied to every stored provider secret.
///
/// After-first-unlock lets bounded background completion read the secret while the device is locked; the
/// device-only variant keeps it out of device migration. P11 documents the lock matrix this choice relies on.
enum KeychainAccessibility: Equatable {
    case afterFirstUnlockThisDeviceOnly

    var attributeValue: CFString {
        switch self {
        case .afterFirstUnlockThisDeviceOnly:
            return kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        }
    }
}

/// Identity of one Keychain item. Contains no secret material.
struct KeychainItemKey: Hashable {
    let service: String
    let account: String
}

struct KeychainReadResult {
    let status: OSStatus
    let secret: Data?
}

/// Narrow seam over the Security calls the credential service needs. Production uses `SecurityKeychain`;
/// tests inject a fake to produce exact `OSStatus` outcomes that the simulator cannot be made to return.
protocol KeychainBoundary {
    func insert(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus
    func replace(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus
    /// Reports whether an item exists and its metadata is readable. Returns only an `OSStatus`, so the health query
    /// can never receive secret bytes.
    func inspect(key: KeychainItemKey) -> OSStatus
    /// Reads the secret. Reserved for dispatch-time resolution.
    func read(key: KeychainItemKey) -> KeychainReadResult
    func remove(key: KeychainItemKey) -> OSStatus
}

/// The real data-protection Keychain. Items use the app's own default access group only, are never synchronized,
/// and queries never include the accessibility class so an item stored under another class is still found.
struct SecurityKeychain: KeychainBoundary {
    private func identityQuery(for key: KeychainItemKey) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: key.service,
            kSecAttrAccount as String: key.account,
        ]
    }

    func insert(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus {
        var attributes = identityQuery(for: key)
        attributes[kSecValueData as String] = secret
        attributes[kSecAttrAccessible as String] = accessibility.attributeValue
        attributes[kSecAttrSynchronizable as String] = false
        return SecItemAdd(attributes as CFDictionary, nil)
    }

    func replace(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus {
        let changes: [String: Any] = [
            kSecValueData as String: secret,
            kSecAttrAccessible as String: accessibility.attributeValue,
        ]
        return SecItemUpdate(identityQuery(for: key) as CFDictionary, changes as CFDictionary)
    }

    func inspect(key: KeychainItemKey) -> OSStatus {
        var query = identityQuery(for: key)
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        return SecItemCopyMatching(query as CFDictionary, &result)
    }

    func read(key: KeychainItemKey) -> KeychainReadResult {
        var query = identityQuery(for: key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess else {
            return KeychainReadResult(status: status, secret: nil)
        }
        return KeychainReadResult(status: status, secret: result as? Data)
    }

    func remove(key: KeychainItemKey) -> OSStatus {
        SecItemDelete(identityQuery(for: key) as CFDictionary)
    }
}
