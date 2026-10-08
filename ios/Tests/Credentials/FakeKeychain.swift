import Foundation
import Security
@testable import OhAndServices

enum FakeKeychainOperation: Hashable {
    case insert
    case replace
    case inspect
    case read
    case remove
}

/// In-memory `KeychainBoundary` for tests. Holds synthetic secrets only. Faults are injected per operation, either
/// for every call (`forcedStatus`) or once each in order (`scriptedStatuses`).
final class FakeKeychain: KeychainBoundary {
    struct StoredItem {
        var secret: Data
        var accessibility: KeychainAccessibility
    }

    private(set) var items: [KeychainItemKey: StoredItem] = [:]
    private(set) var callCounts: [FakeKeychainOperation: Int] = [:]
    var forcedStatus: [FakeKeychainOperation: OSStatus] = [:]
    var scriptedStatuses: [FakeKeychainOperation: [OSStatus]] = [:]

    func plant(key: KeychainItemKey, secret: Data) {
        items[key] = StoredItem(secret: secret, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    func callCount(_ operation: FakeKeychainOperation) -> Int {
        callCounts[operation] ?? 0
    }

    private func injectedStatus(for operation: FakeKeychainOperation) -> OSStatus? {
        callCounts[operation, default: 0] += 1
        if var scripted = scriptedStatuses[operation], !scripted.isEmpty {
            let next = scripted.removeFirst()
            scriptedStatuses[operation] = scripted
            return next
        }
        return forcedStatus[operation]
    }

    func insert(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus {
        if let injected = injectedStatus(for: .insert) { return injected }
        if items[key] != nil { return errSecDuplicateItem }
        items[key] = StoredItem(secret: secret, accessibility: accessibility)
        return errSecSuccess
    }

    func replace(key: KeychainItemKey, secret: Data, accessibility: KeychainAccessibility) -> OSStatus {
        if let injected = injectedStatus(for: .replace) { return injected }
        if items[key] == nil { return errSecItemNotFound }
        items[key] = StoredItem(secret: secret, accessibility: accessibility)
        return errSecSuccess
    }

    func inspect(key: KeychainItemKey) -> OSStatus {
        if let injected = injectedStatus(for: .inspect) { return injected }
        return items[key] == nil ? errSecItemNotFound : errSecSuccess
    }

    func read(key: KeychainItemKey) -> KeychainReadResult {
        if let injected = injectedStatus(for: .read) {
            return KeychainReadResult(status: injected, secret: nil)
        }
        guard let item = items[key] else {
            return KeychainReadResult(status: errSecItemNotFound, secret: nil)
        }
        return KeychainReadResult(status: errSecSuccess, secret: item.secret)
    }

    func remove(key: KeychainItemKey) -> OSStatus {
        if let injected = injectedStatus(for: .remove) { return injected }
        if items.removeValue(forKey: key) == nil { return errSecItemNotFound }
        return errSecSuccess
    }
}
