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

/// Callback function type for core operations.
public typealias CoreCallback = @convention(c) (OpaquePointer?) -> Void

/// C bridge functions (opaque handle interface).
@_silgen_name("ohand_create_handle")
private func _ohand_create_handle() -> CoreHandle

@_silgen_name("ohand_destroy_handle")
private func _ohand_destroy_handle(_ handle: CoreHandle)

@_silgen_name("ohand_is_handle_cancelled")
private func _ohand_is_handle_cancelled(_ handle: CoreHandle) -> Int32

@_silgen_name("ohand_cancel_handle")
private func _ohand_cancel_handle(_ handle: CoreHandle) -> Int32

@_silgen_name("ohand_register_callback")
private func _ohand_register_callback(_ handle: CoreHandle, _ callback: @escaping CoreCallback) -> Int32

@_silgen_name("ohand_invoke_callback")
private func _ohand_invoke_callback(_ handle: CoreHandle, _ closure: OpaquePointer?) -> Int32

@_silgen_name("ohand_classify_error")
private func _ohand_classify_error(_ errorStr: UnsafePointer<CChar>) -> Int32

/// Safe Swift wrapper for core handles.
/// Manages lifetime and provides cancellation.
/// Handles are single-owner; only one copy may exist at a time.
public class OhAndCoreHandle {
    private var handle: CoreHandle?
    private let lock = NSLock()

    public init() {
        self.handle = _ohand_create_handle()
    }

    deinit {
        destroy()
    }

    /// Destroy the handle, releasing resources.
    /// Safe to call multiple times.
    public func destroy() {
        lock.lock()
        defer { lock.unlock() }

        guard let h = handle else { return }
        _ohand_destroy_handle(h)
        handle = nil
    }

    /// Check if this handle is cancelled.
    /// Returns nil if handle is invalid.
    public func isCancelled() -> Bool? {
        lock.lock()
        defer { lock.unlock() }

        guard let h = handle else { return nil }
        let result = _ohand_is_handle_cancelled(h)
        return result == 1 ? true : (result == 0 ? false : nil)
    }

    /// Cancel this handle, preventing future callbacks and operations.
    /// Returns true if successfully cancelled, false if already cancelled or invalid.
    @discardableResult
    public func cancel() -> Bool {
        lock.lock()
        defer { lock.unlock() }

        guard let h = handle else { return false }
        let result = _ohand_cancel_handle(h)
        return result == 0
    }

    /// Register a callback with this handle.
    /// The callback will be invoked when background operations deliver results.
    /// Returns true if successfully registered, false if handle is invalid.
    @discardableResult
    public func registerCallback(_ callback: @escaping CoreCallback) -> Bool {
        lock.lock()
        defer { lock.unlock() }

        guard let h = handle else { return false }
        let result = _ohand_register_callback(h, callback)
        return result == 0
    }

    /// Invoke the registered callback if one is registered and the handle is not cancelled.
    /// - Returns: 0 if callback was invoked, 1 if cancelled, 2 if no callback registered, nil if handle is invalid.
    public func invokeCallback(closure: OpaquePointer? = nil) -> Int32? {
        lock.lock()
        defer { lock.unlock() }

        guard let h = handle else { return nil }
        return _ohand_invoke_callback(h, closure)
    }

    /// Classify an error message into a normalized error class.
    /// - Parameter errorMessage: A simple error message string.
    /// - Returns: A CoreErrorClass value.
    public static func classifyError(_ errorMessage: String) -> CoreErrorClass {
        let result = _ohand_classify_error(errorMessage)
        return CoreErrorClass(rawValue: result) ?? .permanent
    }
}

public struct OhAndCore {
    public init() {}

    public static let version = "0.1.0"

    public func placeholder() -> String {
        "Core framework placeholder for M1"
    }
}
