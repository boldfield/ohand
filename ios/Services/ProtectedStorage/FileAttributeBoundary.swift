import Foundation

/// Narrow seam over the file-system attribute calls the storage service needs. Production uses
/// `FileManagerAttributeBoundary`; tests inject a recording or failing boundary to produce setup failures that a
/// simulator cannot be made to return. The seam has no removal or move operation, so the service cannot delete data.
protocol FileAttributeBoundary {
    func createDirectory(at url: URL) throws
    func setProtection(_ protection: FileProtectionType, at url: URL) throws
    func setExcludedFromBackup(_ excluded: Bool, at url: URL) throws
    /// Every file and directory below `directory`, at any depth. Empty when the directory is empty.
    func descendants(of directory: URL) throws -> [URL]
}

struct FileManagerAttributeBoundary: FileAttributeBoundary {
    func createDirectory(at url: URL) throws {
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }

    func setProtection(_ protection: FileProtectionType, at url: URL) throws {
        try FileManager.default.setAttributes([.protectionKey: protection], ofItemAtPath: url.path)
    }

    func setExcludedFromBackup(_ excluded: Bool, at url: URL) throws {
        var values = URLResourceValues()
        values.isExcludedFromBackup = excluded
        var mutableURL = url
        try mutableURL.setResourceValues(values)
    }

    func descendants(of directory: URL) throws -> [URL] {
        try FileManager.default.subpathsOfDirectory(atPath: directory.path).map {
            directory.appendingPathComponent($0)
        }
    }
}
