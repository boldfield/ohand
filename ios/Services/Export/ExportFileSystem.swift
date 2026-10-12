import Foundation

/// Narrow seam over the files an export touches, all of them inside the protected temporary store. It has no operation
/// that can reach the source stores: the service only passes it URLs under the export directory, and reads the audio
/// source only through `copyFile`.
protocol ExportFileSystem {
    /// Creates `url` (and any missing parents) with the temporary store's protection class, excluded from backup.
    func createPrivateDirectory(at url: URL) throws
    /// Applies the temporary store's protection class to an existing file.
    func protectFile(at url: URL) throws
    /// Copies `source` to `destination` and protects the copy. Throws instead of replacing an existing destination.
    func copyFile(from source: URL, to destination: URL) throws
    func fileExists(at url: URL) -> Bool
    func entryNames(in directory: URL) throws -> [String]
    func remove(at url: URL) throws
}

struct FileManagerExportFileSystem: ExportFileSystem {
    private let protection = StoreProtectionPolicy.policy(for: .temporaryFiles).fileProtection

    func createPrivateDirectory(at url: URL) throws {
        try FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true, attributes: [.protectionKey: protection])
        try FileManager.default.setAttributes([.protectionKey: protection], ofItemAtPath: url.path)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableURL = url
        try mutableURL.setResourceValues(values)
    }

    func protectFile(at url: URL) throws {
        try FileManager.default.setAttributes([.protectionKey: protection], ofItemAtPath: url.path)
    }

    func copyFile(from source: URL, to destination: URL) throws {
        try FileManager.default.copyItem(at: source, to: destination)
        try protectFile(at: destination)
    }

    func fileExists(at url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    func entryNames(in directory: URL) throws -> [String] {
        try FileManager.default.contentsOfDirectory(atPath: directory.path)
    }

    func remove(at url: URL) throws {
        try FileManager.default.removeItem(at: url)
    }
}
