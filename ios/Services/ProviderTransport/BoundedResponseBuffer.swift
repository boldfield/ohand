import Foundation

/// Accumulates a response body without ever holding more than `limit` bytes: a chunk that would cross the limit is
/// refused before it is copied, so memory use is bounded by the caller's ceiling rather than by callback chunk size.
struct BoundedResponseBuffer {
    let limit: Int
    private(set) var contents = Data()

    init(limit: Int) {
        self.limit = max(0, limit)
    }

    var count: Int { contents.count }

    /// Returns false, leaving the buffer unchanged, when `chunk` does not fit in the remaining capacity.
    mutating func append(_ chunk: Data) -> Bool {
        guard chunk.count <= limit - contents.count else { return false }
        contents.append(chunk)
        return true
    }

    mutating func discard() {
        contents = Data()
    }
}
