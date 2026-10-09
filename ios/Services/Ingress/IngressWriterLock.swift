import Darwin
import Foundation

/// The single-writer guarantee for ingress. The foreground app is the only process that writes staging records or
/// moves audio; the lock makes a second writer (another instance, or a future extension that mistakenly links this
/// code) fail loudly instead of racing the first. The lock file lives in the temporary store, whose protection class
/// stays readable after the first unlock, so the lock can be taken while the device is locked.
final class IngressWriterLock {
    enum AcquisitionError: Error, Equatable {
        case anotherWriterActive
        case unavailable(errnoCode: Int32)
    }

    static let lockFileName = "ingress-writer.lock"

    private let descriptor: Int32

    init(lockFileURL: URL) throws {
        let openedDescriptor = Darwin.open(lockFileURL.path, O_RDWR | O_CREAT, 0o600)
        guard openedDescriptor >= 0 else {
            throw AcquisitionError.unavailable(errnoCode: errno)
        }
        guard flock(openedDescriptor, LOCK_EX | LOCK_NB) == 0 else {
            let failureCode = errno
            Darwin.close(openedDescriptor)
            throw failureCode == EWOULDBLOCK ? AcquisitionError.anotherWriterActive : .unavailable(errnoCode: failureCode)
        }
        descriptor = openedDescriptor
    }

    deinit {
        flock(descriptor, LOCK_UN)
        Darwin.close(descriptor)
    }
}
