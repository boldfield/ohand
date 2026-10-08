import Foundation
import Security

protocol KeychainBoundary {
    func add(_ attributes: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus
    func update(_ query: CFDictionary, _ attributes: CFDictionary) -> OSStatus
    func delete(_ query: CFDictionary) -> OSStatus
    func copyMatching(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus
}

final class RealKeychain: KeychainBoundary {
    func add(_ attributes: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus {
        SecItemAdd(attributes, result)
    }

    func update(_ query: CFDictionary, _ attributes: CFDictionary) -> OSStatus {
        SecItemUpdate(query, attributes)
    }

    func delete(_ query: CFDictionary) -> OSStatus {
        SecItemDelete(query)
    }

    func copyMatching(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus {
        SecItemCopyMatching(query, result)
    }
}

final class FakeKeychain: KeychainBoundary {
    private var items: [String: Data] = [:]
    private let lock = NSLock()

    var nextAddStatus: OSStatus?
    var nextUpdateStatus: OSStatus?
    var nextDeleteStatus: OSStatus?
    var nextCopyMatchingStatus: OSStatus?

    func add(_ attributes: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus {
        if let status = nextAddStatus {
            nextAddStatus = nil
            return status
        }

        lock.lock()
        defer { lock.unlock() }

        guard let dict = attributes as? [String: Any] else {
            return errSecParam
        }

        guard let key = dict[kSecAttrAccount as String] as? String else {
            return errSecParam
        }

        guard let value = dict[kSecValueData as String] as? Data else {
            return errSecParam
        }

        if items[key] != nil {
            return errSecDuplicateItem
        }

        items[key] = value
        return errSecSuccess
    }

    func update(_ query: CFDictionary, _ attributes: CFDictionary) -> OSStatus {
        if let status = nextUpdateStatus {
            nextUpdateStatus = nil
            return status
        }

        lock.lock()
        defer { lock.unlock() }

        guard let queryDict = query as? [String: Any] else {
            return errSecParam
        }

        guard let key = queryDict[kSecAttrAccount as String] as? String else {
            return errSecParam
        }

        guard items[key] != nil else {
            return errSecItemNotFound
        }

        guard let attrDict = attributes as? [String: Any],
              let value = attrDict[kSecValueData as String] as? Data else {
            return errSecParam
        }

        items[key] = value
        return errSecSuccess
    }

    func delete(_ query: CFDictionary) -> OSStatus {
        if let status = nextDeleteStatus {
            nextDeleteStatus = nil
            return status
        }

        lock.lock()
        defer { lock.unlock() }

        guard let queryDict = query as? [String: Any] else {
            return errSecParam
        }

        guard let key = queryDict[kSecAttrAccount as String] as? String else {
            return errSecParam
        }

        items.removeValue(forKey: key)
        return errSecSuccess
    }

    func copyMatching(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus {
        if let status = nextCopyMatchingStatus {
            nextCopyMatchingStatus = nil
            return status
        }

        lock.lock()
        defer { lock.unlock() }

        guard let queryDict = query as? [String: Any] else {
            return errSecParam
        }

        guard let key = queryDict[kSecAttrAccount as String] as? String else {
            return errSecParam
        }

        guard let value = items[key] else {
            return errSecItemNotFound
        }

        if queryDict[kSecReturnData as String] as? Bool == true {
            result?.pointee = value as CFData
        }

        return errSecSuccess
    }

    func clear() {
        lock.lock()
        defer { lock.unlock() }
        items.removeAll()
    }
}
