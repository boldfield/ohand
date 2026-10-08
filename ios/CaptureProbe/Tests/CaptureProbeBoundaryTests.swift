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
            text: "Test capture entry",
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
        XCTAssertEqual(saved.capture.text, "Test capture entry")

        let retrieved = try store.capture(id: captureId)
        XCTAssertEqual(retrieved.captureId, captureId)
        XCTAssertEqual(retrieved.entryLocked, false)
        XCTAssertEqual(retrieved.text, "Test capture entry")
    }

    func testIdempotentSave() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let captureId = "idempotent-test-\(UUID().uuidString)"
        let record = CaptureRecord(
            captureId: captureId,
            text: "Idempotent test entry",
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
        XCTAssertEqual(firstSave.capture.text, "Idempotent test entry")

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
            text: "Locked entry test",
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
        XCTAssertEqual(retrieved.text, "Locked entry test")
    }

    func testCaptureIdStability() throws {
        guard let store = store else {
            XCTFail("Store not initialized")
            return
        }

        let captureId = "stable-id-test-\(UUID().uuidString)"
        let record1 = CaptureRecord(
            captureId: captureId,
            text: "First entry with stable ID",
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

        let saved1 = try store.save(record1)
        XCTAssertEqual(saved1.capture.captureId, captureId)
        XCTAssertFalse(saved1.idempotentReplay)

        let retrieved = try store.capture(id: captureId)
        XCTAssertEqual(retrieved.captureId, captureId)
        XCTAssertEqual(retrieved.text, "First entry with stable ID")
    }

    func testPersistenceAcrossReopen() throws {
        let captureId = "reopen-test-\(UUID().uuidString)"
        let record = CaptureRecord(
            captureId: captureId,
            text: "Entry to persist across reopen",
            audioReference: nil,
            captureInstant: "2026-03-01T10:00:00Z",
            timezoneId: "America/New_York",
            utcOffsetMinutes: -300,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: false,
            createdAt: "2026-03-01T10:00:00Z",
            sessionTopic: nil
        )

        // Save in the initial store
        do {
            guard let store = store else {
                XCTFail("Store not initialized")
                return
            }
            let saved = try store.save(record)
            XCTAssertFalse(saved.idempotentReplay)
            store.close()
        }

        // Reopen the store and verify the record persists
        do {
            let newStore = try ProbeStore()
            defer { newStore.close() }

            let retrieved = try newStore.capture(id: captureId)
            XCTAssertEqual(retrieved.captureId, captureId)
            XCTAssertEqual(retrieved.text, "Entry to persist across reopen")
            XCTAssertEqual(retrieved.entryLocked, false)
            XCTAssertEqual(retrieved.timezoneId, "America/New_York")
        }
    }

    func testRetryIdempotencyAcrossReopen() throws {
        let captureId = "retry-idempotent-\(UUID().uuidString)"
        let record = CaptureRecord(
            captureId: captureId,
            text: "Retry test with stable ID",
            audioReference: nil,
            captureInstant: "2026-03-01T10:30:00Z",
            timezoneId: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-local",
            entryLocked: false,
            createdAt: "2026-03-01T10:30:00Z",
            sessionTopic: nil
        )

        // Initial save
        do {
            guard let store = store else {
                XCTFail("Store not initialized")
                return
            }
            let saved = try store.save(record)
            XCTAssertFalse(saved.idempotentReplay)
            store.close()
        }

        // Retry after reopen: should be idempotent
        do {
            let newStore = try ProbeStore()
            defer { newStore.close() }

            let retry = try newStore.save(record)
            XCTAssertTrue(retry.idempotentReplay, "Retry save should be marked as idempotent replay")
            XCTAssertEqual(retry.capture.captureId, captureId)
            XCTAssertEqual(retry.capture.text, "Retry test with stable ID")
        }
    }
}
