import Foundation

/// Opaque C handle type for core operations.
public typealias CoreHandle = OpaquePointer

/// Error classes from the core.
public enum CoreErrorClass: Int32 {
    case ok = 0
    case transient = 1
    case permanent = 2
    case unauthorized = 3
    case cancelled = 4
    case unsupported = 5
}

/// C bridge functions.
@_silgen_name("ohand_create_handle")
private func _ohand_create_handle() -> CoreHandle

@_silgen_name("ohand_destroy_handle")
private func _ohand_destroy_handle(_ handle: UnsafeMutablePointer<CoreHandle>)

@_silgen_name("ohand_is_handle_cancelled")
private func _ohand_is_handle_cancelled(_ handle: UnsafePointer<CoreHandle>) -> Int32

@_silgen_name("ohand_cancel_handle")
private func _ohand_cancel_handle(_ handle: UnsafePointer<CoreHandle>) -> Int32

/// Safe Swift wrapper for core handles.
/// Manages lifetime, provides cancellation, and prevents double-free.
public class OhAndCoreHandle {
    private var cHandle: CoreHandle?
    private let lock = NSLock()

    public init() {
        self.cHandle = _ohand_create_handle()
    }

    deinit {
        destroy()
    }

    /// Destroy the handle, releasing resources.
    /// Safe to call multiple times.
    public func destroy() {
        lock.lock()
        defer { lock.unlock() }

        guard var handle = cHandle else { return }
        _ohand_destroy_handle(&handle)
        cHandle = nil
    }

    /// Check if this handle is cancelled.
    /// Returns nil if handle is invalid.
    public func isCancelled() -> Bool? {
        lock.lock()
        defer { lock.unlock() }

        guard let handle = cHandle else { return nil }
        let result = withUnsafePointer(to: handle) { ptr in
            _ohand_is_handle_cancelled(ptr)
        }
        return result == 1 ? true : (result == 0 ? false : nil)
    }

    /// Cancel this handle, preventing future callbacks and operations.
    /// Returns true if successfully cancelled, false if already cancelled or invalid.
    @discardableResult
    public func cancel() -> Bool {
        lock.lock()
        defer { lock.unlock() }

        guard let handle = cHandle else { return false }
        let result = withUnsafePointer(to: handle) { ptr in
            _ohand_cancel_handle(ptr)
        }
        return result == 0
    }
}

public struct OhAndCore {
    public init() {}

    public static let version = "0.1.0"

    public func placeholder() -> String {
        "Core framework placeholder for M1"
    }
}
