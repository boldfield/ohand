import Foundation

/// Launch-time proof that a real Swift-to-Rust call works inside the app process. A failure
/// here crashes the probe, which the simulator smoke test reports as a failed launch.
enum BoundarySelfCheck {
    static func run() throws -> String {
        let store = try ProbeStore()
        let text = "Self-check ☕ 日本語 👩‍👩‍👧"
        let saved = try store.save(CaptureRecord.sample(captureId: "self-check-1", text: text))
        let fetched = try store.capture(id: "self-check-1")
        guard Array((saved.capture.text ?? "").utf8) == Array(text.utf8), fetched == saved.capture else {
            throw BoundaryFailure(errorClass: .permanent, code: "self_check_mismatch", message: "round trip differs")
        }

        var observedFailure: BoundaryFailure?
        do {
            _ = try store.saveRaw([0xFF, 0xFE])
        } catch let failure as BoundaryFailure {
            observedFailure = failure
        }
        guard let failure = observedFailure, failure.code == "invalid_utf8" else {
            throw BoundaryFailure(errorClass: .permanent, code: "self_check_no_failure", message: "invalid UTF-8 not rejected")
        }
        return "Rust core round trip OK (ABI \(ohand_bindings_abi_version()), failure class \(failure.errorClass.rawValue))"
    }
}
