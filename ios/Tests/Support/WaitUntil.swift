import Foundation
import XCTest

/// Bounded waits for asserted state, replacing fixed-count `Task.yield()` loops and fixed sleeps. A fixed wait ends
/// when a count runs out, which on a loaded simulator can be before the work it waits for; these end when the
/// condition holds, and fail the test naming the condition when it does not within `timeout` seconds.

/// Waits from an async test, yielding and sleeping briefly between checks so the work being waited for can run.
/// The condition runs on the caller's actor, so a `@MainActor` test may read main-actor state directly.
func waitUntil(
    _ condition: String,
    timeout: TimeInterval = 5,
    file: StaticString = #filePath,
    line: UInt = #line,
    isolation: isolated (any Actor)? = #isolation,
    _ holds: () -> Bool
) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !holds() {
        if Date() >= deadline {
            XCTFail("timed out after \(timeout) s waiting for: \(condition)", file: file, line: line)
            return
        }
        await Task.yield()
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
}

/// Waits from a synchronous test by turning the run loop, which main-queue and run-loop work needs in order to run.
func waitUntilPumping(
    _ condition: String,
    timeout: TimeInterval = 5,
    file: StaticString = #filePath,
    line: UInt = #line,
    _ holds: () -> Bool
) {
    let deadline = Date().addingTimeInterval(timeout)
    while !holds() {
        if Date() >= deadline {
            XCTFail("timed out after \(timeout) s waiting for: \(condition)", file: file, line: line)
            return
        }
        RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
    }
}
