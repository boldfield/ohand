import Darwin
import Foundation

/// The file operations ingress needs. Production uses `FileManagerIngressFileSystem`; tests inject a wrapper that fails
/// selected operations to prove that an interrupted step never loses an acknowledged source.
protocol IngressFileSystem {
    func fileExists(at url: URL) -> Bool
    func fileSize(at url: URL) throws -> Int
    /// File names directly inside `directory`; empty when it has no entries.
    func entryNames(in directory: URL) throws -> [String]
    func read(from url: URL) throws -> Data
    /// Writes `data` so that either the complete file exists at `url` or nothing does, and throws instead of
    /// replacing a file that is already there. The data is flushed to storage before the file becomes visible.
    func writeDurably(_ data: Data, to url: URL) throws
    /// Renames `source` to `destination` within one volume, so the file is never in both places and never partial.
    /// Throws instead of replacing an existing destination.
    func move(from source: URL, to destination: URL) throws
    func remove(at url: URL) throws
}

enum IngressFileSystemNaming {
    /// Suffix of a write that has not become a record yet. Such files are never imported and never deleted.
    static let incompleteWriteSuffix = ".incomplete"
}

struct FileManagerIngressFileSystem: IngressFileSystem {
    func fileExists(at url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    func fileSize(at url: URL) throws -> Int {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return (attributes[.size] as? NSNumber)?.intValue ?? 0
    }

    func entryNames(in directory: URL) throws -> [String] {
        try FileManager.default.contentsOfDirectory(atPath: directory.path)
    }

    func read(from url: URL) throws -> Data {
        try Data(contentsOf: url)
    }

    func writeDurably(_ data: Data, to url: URL) throws {
        if fileExists(at: url) {
            throw CocoaError(.fileWriteFileExists)
        }
        let finalPath = url.path
        let incompletePath = finalPath + IngressFileSystemNaming.incompleteWriteSuffix
        let descriptor = Darwin.open(incompletePath, O_WRONLY | O_CREAT | O_TRUNC, 0o600)
        guard descriptor >= 0 else { throw Self.posixError() }

        do {
            try Self.writeAll(data, to: descriptor)
            try Self.flush(descriptor)
        } catch {
            Darwin.close(descriptor)
            try? FileManager.default.removeItem(atPath: incompletePath)
            throw error
        }
        guard Darwin.close(descriptor) == 0 else {
            let failure = Self.posixError()
            try? FileManager.default.removeItem(atPath: incompletePath)
            throw failure
        }
        guard Darwin.rename(incompletePath, finalPath) == 0 else {
            let failure = Self.posixError()
            try? FileManager.default.removeItem(atPath: incompletePath)
            throw failure
        }
        Self.flushDirectory(url.deletingLastPathComponent())
    }

    func move(from source: URL, to destination: URL) throws {
        try FileManager.default.moveItem(at: source, to: destination)
        Self.flushDirectory(destination.deletingLastPathComponent())
    }

    func remove(at url: URL) throws {
        try FileManager.default.removeItem(at: url)
    }

    private static func posixError() -> Error {
        POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }

    private static func writeAll(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
            var offset = 0
            while offset < buffer.count {
                let written = Darwin.write(descriptor, buffer.baseAddress! + offset, buffer.count - offset)
                if written < 0 {
                    if errno == EINTR { continue }
                    throw posixError()
                }
                offset += written
            }
        }
    }

    private static func flush(_ descriptor: Int32) throws {
        if fcntl(descriptor, F_FULLFSYNC) == 0 { return }
        guard fsync(descriptor) == 0 else { throw posixError() }
    }

    /// Best effort: the file itself is already flushed, and a directory of a locked device may not be openable.
    private static func flushDirectory(_ directory: URL) {
        let descriptor = Darwin.open(directory.path, O_RDONLY)
        guard descriptor >= 0 else { return }
        _ = fsync(descriptor)
        Darwin.close(descriptor)
    }
}
