import XCTest

final class BoundaryTests: XCTestCase {
    private func assertFailure<Output>(
        _ expectedCode: String,
        _ expectedClass: BoundaryErrorClass = .permanent,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ operation: () throws -> Output
    ) {
        do {
            _ = try operation()
            XCTFail("expected failure \(expectedCode)", file: file, line: line)
        } catch let failure as BoundaryFailure {
            XCTAssertEqual(failure.code, expectedCode, file: file, line: line)
            XCTAssertEqual(failure.errorClass, expectedClass, file: file, line: line)
        } catch {
            XCTFail("unexpected error \(error)", file: file, line: line)
        }
    }

    private func encodedRequest(_ record: CaptureRecord) throws -> [UInt8] {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return [UInt8](try encoder.encode(record))
    }

    private func requestObject(_ record: CaptureRecord) throws -> [String: Any] {
        let data = try JSONEncoder().encode(record)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func testAbiVersionAndLimitMatchTheGeneratedHeader() {
        XCTAssertEqual(ohand_bindings_abi_version(), UInt32(OHAND_BINDINGS_ABI_VERSION))
        XCTAssertEqual(boundaryMaxRequestBytes(), Int(OHAND_MAX_REQUEST_BYTES))
        XCTAssertEqual(boundaryMaxRequestBytes(), 1_048_576)
    }

    func testCaptureShapedValueRoundTripsThroughRealCore() throws {
        let store = try ProbeStore()
        var record = CaptureRecord.sample(captureId: "cap-1", text: "Call the roofer tomorrow at 9")
        record.sessionTopic = "house"
        let saved = try store.save(record)
        XCTAssertEqual(saved.capture, record)
        XCTAssertFalse(saved.idempotentReplay)
        XCTAssertEqual(try store.capture(id: "cap-1"), record)
    }

    func testUnicodeSurvivesByteForByte() throws {
        let store = try ProbeStore()
        let text = "Café ☕ 日本語 \u{202E}rtl\u{202C} 👩‍👩‍👧 e\u{301} nul:\u{0} end"
        _ = try store.save(CaptureRecord.sample(captureId: "cap-unicode", text: text))
        let fetched = try store.capture(id: "cap-unicode")
        XCTAssertEqual(Array((fetched.text ?? "").utf8), Array(text.utf8))
        XCTAssertTrue((fetched.text ?? "").utf8.contains(0), "an interior NUL does not truncate the text")
    }

    func testIdenticalRetryIsIdempotentAndConflictKeepsOriginalWords() throws {
        let store = try ProbeStore()
        let original = CaptureRecord.sample(captureId: "cap-1", text: "original words")
        XCTAssertFalse(try store.save(original).idempotentReplay)
        XCTAssertTrue(try store.save(original).idempotentReplay)
        assertFailure("capture_conflict") {
            try store.save(CaptureRecord.sample(captureId: "cap-1", text: "different words"))
        }
        XCTAssertEqual(try store.capture(id: "cap-1").text, "original words")
    }

    func testMalformedInputsReturnNormalizedFailures() throws {
        let store = try ProbeStore()
        let valid = CaptureRecord.sample(captureId: "cap-1", text: "x")

        assertFailure("invalid_utf8") { try store.saveRaw([0xFF, 0xFE, 0x7B]) }
        assertFailure("invalid_request") { try store.saveRaw(Array("not json".utf8)) }
        assertFailure("invalid_request") { try store.saveRaw([]) }

        var missingField = try requestObject(valid)
        missingField.removeValue(forKey: "timezone_id")
        assertFailure("invalid_request") {
            try store.saveRaw([UInt8](try JSONSerialization.data(withJSONObject: missingField)))
        }

        var unknownField = try requestObject(valid)
        unknownField["surprise"] = 1
        assertFailure("invalid_request") {
            try store.saveRaw([UInt8](try JSONSerialization.data(withJSONObject: unknownField)))
        }

        var noSource = valid
        noSource.text = nil
        assertFailure("invalid_request") { try store.save(noSource) }
        assertFailure("invalid_request") { try store.save(CaptureRecord.sample(captureId: "", text: "x")) }

        assertFailure("not_found") { try store.capture(id: "missing") }
        assertFailure("invalid_request") {
            try store.saveRaw(Array("{\"text\":\"private-phrase\"}".utf8))
        }
    }

    func testFailurePayloadsNeverEchoCallerContent() throws {
        let store = try ProbeStore()
        let secret = "do-not-echo-this-private-phrase"
        do {
            _ = try store.saveRaw(Array("{\"text\":\"\(secret)\"}".utf8))
            XCTFail("expected failure")
        } catch let failure as BoundaryFailure {
            XCTAssertFalse(failure.message.contains(secret))
            XCTAssertFalse(failure.code.contains(secret))
        }
    }

    func testNullPointersAreRejectedNotDereferenced() throws {
        let store = try ProbeStore()
        assertFailure("null_argument") {
            try store.withHandle { openHandle in
                try consumeResult(ohand_probe_save_capture(openHandle, nil, nil, 5))
            }
        }
        assertFailure("null_argument") {
            try consumeResult(ohand_probe_save_capture(nil, nil, [0x7B, 0x7D], 2))
        }
    }

    func testSizeBoundIsEnforcedExactly() throws {
        let store = try ProbeStore()
        let limit = boundaryMaxRequestBytes()
        let overhead = try encodedRequest(CaptureRecord.sample(captureId: "cap-limit", text: "")).count

        let atLimitText = String(repeating: "a", count: limit - overhead)
        let atLimit = try encodedRequest(CaptureRecord.sample(captureId: "cap-limit", text: atLimitText))
        XCTAssertEqual(atLimit.count, limit)
        let accepted = try store.saveRaw(atLimit)
        XCTAssertFalse(accepted.isEmpty)
        XCTAssertEqual(try store.capture(id: "cap-limit").text?.utf8.count, atLimitText.utf8.count)

        let overLimit = try encodedRequest(CaptureRecord.sample(captureId: "cap-overx", text: atLimitText + "a"))
        XCTAssertEqual(overLimit.count, limit + 1)
        assertFailure("request_too_large") { try store.saveRaw(overLimit) }
        assertFailure("not_found") { try store.capture(id: "cap-overx") }

        let multibyteText = String(repeating: "é", count: (limit - overhead) / 2 + 1)
        let multibyte = try encodedRequest(CaptureRecord.sample(captureId: "cap-multibyte", text: multibyteText))
        XCTAssertGreaterThan(multibyte.count, limit)
        assertFailure("request_too_large") { try store.saveRaw(multibyte) }
    }

    func testOversizedLengthIsRejectedWithoutReadingMemory() throws {
        let store = try ProbeStore()
        let bogusPointer = UnsafePointer<UInt8>(bitPattern: 1)
        assertFailure("request_too_large") {
            try store.withHandle { openHandle in
                try consumeResult(ohand_probe_save_capture(openHandle, nil, bogusPointer, boundaryMaxRequestBytes() + 1))
            }
        }
    }

    func testCancellationBeforeCallChangesNothing() throws {
        let store = try ProbeStore()
        let token = CancelToken()
        token.cancel()
        assertFailure("cancelled", .cancelled) {
            try store.save(CaptureRecord.sample(captureId: "cap-1", text: "x"), cancelToken: token)
        }
        assertFailure("not_found") { try store.capture(id: "cap-1") }
        assertFailure("cancelled", .cancelled) { try store.capture(id: "cap-1", cancelToken: token) }
    }

    func testCancellationAfterCompletionDoesNotUndoACommittedCapture() throws {
        let store = try ProbeStore()
        let token = CancelToken()
        _ = try store.save(CaptureRecord.sample(captureId: "cap-1", text: "kept"), cancelToken: token)
        token.cancel()
        token.cancel()
        XCTAssertEqual(try store.capture(id: "cap-1").text, "kept")
    }

    func testConcurrentCancellationIsAllOrNothing() throws {
        let store = try ProbeStore()
        let recorder = OutcomeRecorder()
        DispatchQueue.concurrentPerform(iterations: 200) { index in
            let token = CancelToken()
            let cancellerFinished = DispatchGroup()
            DispatchQueue.global().async(group: cancellerFinished) { token.cancel() }
            defer { cancellerFinished.wait() }
            let captureId = "cap-\(index)"
            do {
                _ = try store.save(CaptureRecord.sample(captureId: captureId, text: "text"), cancelToken: token)
                recorder.record(captureId, committed: true)
            } catch let failure as BoundaryFailure where failure.errorClass == .cancelled {
                recorder.record(captureId, committed: false)
            } catch {
                recorder.recordUnexpected("\(error)")
            }
        }
        XCTAssertTrue(recorder.unexpected.isEmpty, "unexpected errors: \(recorder.unexpected)")
        XCTAssertEqual(recorder.outcomes.count, 200)
        for (captureId, committed) in recorder.outcomes {
            let stored = (try? store.capture(id: captureId)) != nil
            XCTAssertEqual(stored, committed, "\(captureId): result disagrees with the store")
        }
    }

    func testOwnershipIsReleasedOnEveryPath() throws {
        let baseline = boundaryLiveAllocations()
        do {
            let store = try ProbeStore()
            let token = CancelToken()
            XCTAssertEqual(boundaryLiveAllocations(), baseline + 2)

            for index in 0..<100 {
                _ = try? store.save(CaptureRecord.sample(captureId: "cap-\(index)", text: "ok"), cancelToken: token)
                _ = try? store.saveRaw(Array("garbage".utf8), cancelToken: token)
                _ = try? store.capture(id: "missing")
                _ = try? consumeResult(ohand_probe_trigger_panic())
            }
            token.cancel()
            _ = try? store.save(CaptureRecord.sample(captureId: "cap-cancelled", text: "x"), cancelToken: token)
            XCTAssertEqual(boundaryLiveAllocations(), baseline + 2, "no result buffer outlives its call")

            var heldResult = ohand_probe_trigger_panic()
            XCTAssertEqual(boundaryLiveAllocations(), baseline + 3, "an unconsumed result stays allocated")
            ohand_result_free(&heldResult)
            ohand_result_free(&heldResult)
            XCTAssertNil(heldResult.data)
            XCTAssertEqual(boundaryLiveAllocations(), baseline + 2)

            ohand_probe_store_free(nil)
            ohand_cancel_token_free(nil)
            ohand_cancel_token_cancel(nil)
            ohand_result_free(nil)
            withExtendedLifetime((store, token)) {}
        }
        XCTAssertEqual(boundaryLiveAllocations(), baseline, "deinit released the store and the token")
    }

    func testStoreLifetimeAndIsolation() throws {
        let first = try ProbeStore()
        let second = try ProbeStore()
        _ = try first.save(CaptureRecord.sample(captureId: "cap-1", text: "only in first"))
        assertFailure("not_found") { try second.capture(id: "cap-1") }

        first.close()
        first.close()
        assertFailure("store_closed") { try first.capture(id: "cap-1") }
        assertFailure("store_closed") { try first.save(CaptureRecord.sample(captureId: "cap-2", text: "x")) }
        assertFailure("not_found") { try second.capture(id: "cap-1") }
    }

    func testConcurrentSavesAndCloseNeverUseAFreedHandle() throws {
        let store = try ProbeStore()
        DispatchQueue.concurrentPerform(iterations: 100) { index in
            if index == 50 {
                store.close()
            } else {
                _ = try? store.save(CaptureRecord.sample(captureId: "cap-\(index)", text: "x"))
            }
        }
        assertFailure("store_closed") { try store.capture(id: "cap-0") }
    }

    func testConcurrentSavesFromManyThreadsAllPersist() throws {
        let store = try ProbeStore()
        let recorder = OutcomeRecorder()
        DispatchQueue.concurrentPerform(iterations: 200) { index in
            do {
                _ = try store.save(CaptureRecord.sample(captureId: "cap-\(index)", text: "text \(index)"))
            } catch {
                recorder.recordUnexpected("\(error)")
            }
        }
        XCTAssertTrue(recorder.unexpected.isEmpty, "unexpected errors: \(recorder.unexpected)")
        for index in 0..<200 {
            XCTAssertEqual(try store.capture(id: "cap-\(index)").text, "text \(index)")
        }
    }

    func testRustPanicIsContainedAsAnInternalFailure() throws {
        assertFailure("internal") { try consumeResult(ohand_probe_trigger_panic()) }
        let store = try ProbeStore()
        XCTAssertFalse(try store.save(CaptureRecord.sample(captureId: "after-panic", text: "x")).idempotentReplay)
    }
}

private final class OutcomeRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var recordedOutcomes: [String: Bool] = [:]
    private var recordedUnexpected: [String] = []

    func record(_ captureId: String, committed: Bool) {
        lock.lock()
        defer { lock.unlock() }
        recordedOutcomes[captureId] = committed
    }

    func recordUnexpected(_ description: String) {
        lock.lock()
        defer { lock.unlock() }
        recordedUnexpected.append(description)
    }

    var outcomes: [String: Bool] {
        lock.lock()
        defer { lock.unlock() }
        return recordedOutcomes
    }

    var unexpected: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recordedUnexpected
    }
}
