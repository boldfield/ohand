import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// The production `AppServices` hands Status the failed-work read, the private text read and the unlock request. The
/// fixture launch builds its services through the same `AppServices` initializer, with scripted adapters.
final class StatusProductionWiringTests: XCTestCase {
    private final class Outcome: @unchecked Sendable {
        var failedNotes: [FailedNote] = []
        var firstLine: String?
        var access: FailedNotesAccess = .notRead
        var failure: Error?
    }

    func testTheAssembledStatusModelListsAFailedNoteWithItsFirstLine() throws {
        let launch = UITestFixtureLaunch(name: "failed-sorting")
        let services = try launch.makeServices()
        services.start(isActive: true)
        defer { services.shutDown() }
        let outcome = Outcome()
        let finished = expectation(description: "the assembled status model read the failed note")
        Task { @MainActor in
            do {
                _ = try await launch.seedAndWait()
                let model = try XCTUnwrap(services.managementContext.statusModel)
                await model.refresh()
                outcome.access = model.failedNotesAccess
                outcome.failedNotes = model.failedNotes
                if let note = model.failedNotes.first {
                    await model.ensureFirstLine(itemID: note.itemID)
                    outcome.firstLine = model.firstLines[note.itemID]
                }
            } catch {
                outcome.failure = error
            }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 180)
        XCTAssertNil(outcome.failure)
        XCTAssertEqual(outcome.access, .readable)
        XCTAssertEqual(outcome.failedNotes.count, 1)
        XCTAssertEqual(outcome.firstLine, "Compare the synthetic rejected quotes for the shed roof")
    }
}
