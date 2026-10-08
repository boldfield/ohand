import Foundation
import Security

enum KeychainTester {
    static let syntheticServiceName = "com.boldfield.ohand.probes.credential.test"
    static let decoyServiceName = "com.boldfield.ohand.probes.credential.decoy"
    static let syntheticAccountPrefix = "test-credential"

    enum AccessibilityClass: CaseIterable {
        case whenUnlocked
        case afterFirstUnlock
        case afterFirstUnlockThisDeviceOnly
        case whenUnlockedThisDeviceOnly
        case whenPasscodeSetThisDeviceOnly

        var keychainValue: CFString {
            switch self {
            case .whenUnlocked:
                return kSecAttrAccessibleWhenUnlocked
            case .afterFirstUnlock:
                return kSecAttrAccessibleAfterFirstUnlock
            case .afterFirstUnlockThisDeviceOnly:
                return kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            case .whenUnlockedThisDeviceOnly:
                return kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            case .whenPasscodeSetThisDeviceOnly:
                return kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
            }
        }

        var displayName: String {
            switch self {
            case .whenUnlocked:
                return "WhenUnlocked"
            case .afterFirstUnlock:
                return "AfterFirstUnlock"
            case .afterFirstUnlockThisDeviceOnly:
                return "AfterFirstUnlockThisDeviceOnly"
            case .whenUnlockedThisDeviceOnly:
                return "WhenUnlockedThisDeviceOnly"
            case .whenPasscodeSetThisDeviceOnly:
                return "WhenPasscodeSetThisDeviceOnly"
            }
        }
    }

    static func accountName(for accessibility: AccessibilityClass) -> String {
        return "\(syntheticAccountPrefix)-\(accessibility.displayName)"
    }

    static func syntheticValue(for accessibility: AccessibilityClass) -> String {
        return "synthetic-credential-\(accessibility.displayName)"
    }

    static func statusText(_ status: OSStatus) -> String {
        let name: String
        switch status {
        case errSecSuccess:
            name = "errSecSuccess"
        case errSecItemNotFound:
            name = "errSecItemNotFound"
        case errSecDuplicateItem:
            name = "errSecDuplicateItem"
        case errSecInteractionNotAllowed:
            name = "errSecInteractionNotAllowed"
        case errSecMissingEntitlement:
            name = "errSecMissingEntitlement"
        default:
            name = "other"
        }
        return "\(status) (\(name))"
    }

    private static func identityQuery(service: String, account: String) -> [String: Any] {
        return [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    @discardableResult
    static func deleteCredential(service: String = syntheticServiceName, account: String) -> OSStatus {
        let status = SecItemDelete(identityQuery(service: service, account: account) as CFDictionary)
        return status == errSecItemNotFound ? errSecSuccess : status
    }

    static func storeCredential(
        value: String,
        accessibility: AccessibilityClass,
        service: String = syntheticServiceName
    ) -> OSStatus {
        let account = accountName(for: accessibility)
        let deleteStatus = deleteCredential(service: service, account: account)
        if deleteStatus != errSecSuccess {
            return deleteStatus
        }
        var addQuery = identityQuery(service: service, account: account)
        addQuery[kSecValueData as String] = Data(value.utf8)
        addQuery[kSecAttrAccessible as String] = accessibility.keychainValue
        return SecItemAdd(addQuery as CFDictionary, nil)
    }

    static func retrieveCredential(
        accessibility: AccessibilityClass,
        service: String = syntheticServiceName
    ) -> (value: String?, status: OSStatus) {
        var query = identityQuery(service: service, account: accountName(for: accessibility))
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let data = result as? Data {
            return (value: String(data: data, encoding: .utf8), status: status)
        }
        return (value: nil, status: status)
    }

    @discardableResult
    static func deleteAllTestCredentials() -> [OSStatus] {
        return AccessibilityClass.allCases.map { accessibility in
            deleteCredential(account: accountName(for: accessibility))
        }
    }
}
