import XCTest

class CaptureProbeBoundaryTests: XCTestCase {
    var store: ProbeStore?

    override func setUp() {
        super.setUp()
        do {
            store = try ProbeStore()
        } catch {
            XCTFail("Failed to create store: \(error)")
        }
    }

    override func tearDown() {
        store?.close()
        store = nil
        super.tearDown()
    }

    func testSaveAndRetrieveCapture() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let captureId = "test-capture-\(UUID().uuidString)"
        let record = CaptureRecord(
            captureId: captureId,
            text: nil,
            audioReference: nil,
            captureInstant: "2026-03-01T09:30:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: false,
            createdAt: "2026-03-01T09:30:00Z",
            sessionTopic: nil
        )

        let saved = try store.save(record)
        XCTAssertEqual(saved.capture.captureId, captureId)
        XCTAssertFalse(saved.idempotentReplay)

        let retrieved = try store.capture(id: captureId)
        XCTAssertEqual(retrieved.captureId, captureId)
        XCTAssertEqual(retrieved.entryLocked, false)
    }

    func testIdempotentSave() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let captureId = "idempotent-test-\(UUID().uuidString)"
        let record = CaptureRecord(
            captureId: captureId,
            text: nil,
            audioReference: nil,
            captureInstant: "2026-03-01T09:30:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: false,
            createdAt: "2026-03-01T09:30:00Z",
            sessionTopic: nil
        )

        let firstSave = try store.save(record)
        XCTAssertFalse(firstSave.idempotentReplay)

        let secondSave = try store.save(record)
        XCTAssertTrue(secondSave.idempotentReplay)
        XCTAssertEqual(secondSave.capture.captureId, captureId)
    }

    func testLockedState() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let captureId = "locked-test-\(UUID().uuidString)"
        let lockedRecord = CaptureRecord(
            captureId: captureId,
            text: nil,
            audioReference: nil,
            captureInstant: "2026-03-01T09:30:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: true,
            createdAt: "2026-03-01T09:30:00Z",
            sessionTopic: nil
        )

        let saved = try store.save(lockedRecord)
        XCTAssertTrue(saved.capture.entryLocked)

        let retrieved = try store.capture(id: captureId)
        XCTAssertTrue(retrieved.entryLocked)
    }

    func testMultipleCapturesWithDifferentIds() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let id1 = "multi-test-\(UUID().uuidString)"
        let id2 = "multi-test-\(UUID().uuidString)"

        let record1 = CaptureRecord(
            captureId: id1,
            text: nil,
            audioReference: nil,
            captureInstant: "2026-03-01T09:30:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: false,
            createdAt: "2026-03-01T09:30:00Z",
            sessionTopic: nil
        )

        let record2 = CaptureRecord(
            captureId: id2,
            text: nil,
            audioReference: nil,
            captureInstant: "2026-03-01T10:00:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: true,
            createdAt: "2026-03-01T10:00:00Z",
            sessionTopic: nil
        )

        try store.save(record1)
        try store.save(record2)

        let retrieved1 = try store.capture(id: id1)
        let retrieved2 = try store.capture(id: id2)

        XCTAssertFalse(retrieved1.entryLocked)
        XCTAssertTrue(retrieved2.entryLocked)
    }
}
