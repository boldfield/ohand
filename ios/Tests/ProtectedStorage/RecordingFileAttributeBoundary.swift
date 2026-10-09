import Foundation
@testable import OhAndServices

/// Records every attribute call and can fail selected ones. With a wrapped boundary the call also reaches the real
/// file system; without one it keeps an in-memory view of the attributes and `simulatedItems` stand in for files.
final class RecordingFileAttributeBoundary: FileAttributeBoundary {
    enum Call: Equatable {
        case createDirectory(String)
        case setProtection(FileProtectionType, String)
        case setExcludedFromBackup(Bool, String)
        case descendants(String)
    }

    private let wrapped: FileAttributeBoundary?
    private(set) var calls: [Call] = []
    private(set) var protectionByPath: [String: FileProtectionType] = [:]
    private(set) var exclusionByPath: [String: Bool] = [:]
    var simulatedItems: [URL] = []
    var shouldFail: (Call) -> Bool = { _ in false }

    init(wrapping wrapped: FileAttributeBoundary? = nil) {
        self.wrapped = wrapped
    }

    private func enter(_ call: Call) throws {
        calls.append(call)
        if shouldFail(call) {
            throw NSError(domain: "synthetic.protected-storage", code: 7)
        }
    }

    func createDirectory(at url: URL) throws {
        try enter(.createDirectory(url.path))
        try wrapped?.createDirectory(at: url)
    }

    func setProtection(_ protection: FileProtectionType, at url: URL) throws {
        try enter(.setProtection(protection, url.path))
        try wrapped?.setProtection(protection, at: url)
        protectionByPath[url.path] = protection
    }

    func setExcludedFromBackup(_ excluded: Bool, at url: URL) throws {
        try enter(.setExcludedFromBackup(excluded, url.path))
        try wrapped?.setExcludedFromBackup(excluded, at: url)
        exclusionByPath[url.path] = excluded
    }

    func descendants(of directory: URL) throws -> [URL] {
        try enter(.descendants(directory.path))
        if let wrapped {
            return try wrapped.descendants(of: directory)
        }
        return simulatedItems.filter { $0.path.hasPrefix(directory.path + "/") }
    }
}
