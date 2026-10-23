import CryptoKit
import Foundation

/// One notification fact or user action waiting to reach the core. It holds only the core-derived identifier, the
/// opaque target and a time; never item text.
struct NotificationActionEntry: Codable, Equatable, Sendable {
    enum Operation: String, Codable, Sendable {
        case recordDelivered = "record_delivered"
        case recordOpened = "record_opened"
        case acknowledge
        case complete
        /// The tap's route to the item, which needs a private read and therefore an authenticated session.
        case open

        /// Replay order for entries with the same time: evidence first, then the user's decisions.
        fileprivate var replayRank: Int {
            switch self {
            case .recordDelivered: return 0
            case .recordOpened: return 1
            case .acknowledge: return 2
            case .complete: return 3
            case .open: return 4
            }
        }
    }

    let operation: Operation
    let notificationID: String
    let targetID: String?
    let occurredAtUnixMilliseconds: Int64

    /// Stable for one operation on one notification, so a duplicate callback for work still waiting coalesces into the
    /// entry that is already stored. Work already settled is handled by the core, which answers the repeat without
    /// changing anything.
    var entryID: String {
        let digest = SHA256.hash(data: Data("\(operation.rawValue)|\(notificationID)".utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }

    var occurredAt: Date { Date(timeIntervalSince1970: Double(occurredAtUnixMilliseconds) / 1000) }

    static func replayOrder(_ lhs: NotificationActionEntry, _ rhs: NotificationActionEntry) -> Bool {
        if lhs.occurredAtUnixMilliseconds != rhs.occurredAtUnixMilliseconds {
            return lhs.occurredAtUnixMilliseconds < rhs.occurredAtUnixMilliseconds
        }
        if lhs.operation.replayRank != rhs.operation.replayRank {
            return lhs.operation.replayRank < rhs.operation.replayRank
        }
        return lhs.entryID < rhs.entryID
    }
}

/// Durable store for entries between the OS callback and the core's answer. `append` returning means the entry will
/// survive a crash or a relaunch; `append` throwing means it will not, and the caller must say so.
protocol NotificationActionJournal: AnyObject, Sendable {
    /// Stores the entry. An entry with the same `entryID` already waiting is success without a second copy.
    func append(_ entry: NotificationActionEntry) throws
    /// The waiting entries in replay order. An entry that cannot be read is left in place and not returned.
    func pending() throws -> [NotificationActionEntry]
    func remove(_ entry: NotificationActionEntry) throws
}

/// Keeps one file per entry in a directory, written with the ingress durable-write discipline: the entry is flushed
/// before it becomes visible, so a crash leaves either the whole entry or none.
final class FileNotificationActionJournal: NotificationActionJournal, @unchecked Sendable {
    private let directory: URL
    private let fileSystem: IngressFileSystem
    private let lock = NSLock()

    init(directory: URL, fileSystem: IngressFileSystem = FileManagerIngressFileSystem()) {
        self.directory = directory
        self.fileSystem = fileSystem
    }

    func append(_ entry: NotificationActionEntry) throws {
        lock.lock()
        defer { lock.unlock() }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = url(for: entry)
        if fileSystem.fileExists(at: url) { return }
        try fileSystem.writeDurably(try JSONEncoder().encode(entry), to: url)
    }

    func pending() throws -> [NotificationActionEntry] {
        lock.lock()
        defer { lock.unlock() }
        guard fileSystem.fileExists(at: directory) else { return [] }
        let decoder = JSONDecoder()
        var entries: [NotificationActionEntry] = []
        for name in try fileSystem.entryNames(in: directory) where name.hasSuffix(Self.entrySuffix) {
            guard let data = try? fileSystem.read(from: directory.appendingPathComponent(name)),
                  let entry = try? decoder.decode(NotificationActionEntry.self, from: data)
            else { continue }
            entries.append(entry)
        }
        return entries.sorted(by: NotificationActionEntry.replayOrder)
    }

    func remove(_ entry: NotificationActionEntry) throws {
        lock.lock()
        defer { lock.unlock() }
        let url = url(for: entry)
        guard fileSystem.fileExists(at: url) else { return }
        try fileSystem.remove(at: url)
    }

    private static let entrySuffix = ".action.json"

    private func url(for entry: NotificationActionEntry) -> URL {
        directory.appendingPathComponent(entry.entryID + Self.entrySuffix)
    }
}
