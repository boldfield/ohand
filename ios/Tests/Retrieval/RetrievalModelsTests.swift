import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

final class RetrievalModelsTests: XCTestCase {
    private var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    private func day(_ year: Int, _ month: Int, _ dayOfMonth: Int, hour: Int = 15) -> Date {
        calendar.date(from: DateComponents(year: year, month: month, day: dayOfMonth, hour: hour))!
    }

    private func answer(_ json: String) throws -> PrivateRetrievalAnswer {
        try JSONDecoder().decode(PrivateRetrievalAnswer.self, from: Data(json.utf8))
    }

    private static let itemID = "0b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"

    // MARK: request

    func testARequestWithoutWordsOrFiltersAsksForSomething() {
        let request = RetrievalSearchRequest(query: "   ")
        XCTAssertFalse(request.hasCriteria)
        XCTAssertNotNil(request.problem(calendar: calendar))
    }

    func testAnyOneFilterIsEnoughToListWithoutWords() {
        XCTAssertTrue(RetrievalSearchRequest(sessionTopic: "garage").hasCriteria)
        XCTAssertTrue(RetrievalSearchRequest(onlyWithSessionTopic: true).hasCriteria)
        XCTAssertTrue(RetrievalSearchRequest(since: day(2026, 10, 1)).hasCriteria)
        XCTAssertTrue(RetrievalSearchRequest(itemType: .idea).hasCriteria)
        XCTAssertFalse(RetrievalSearchRequest(sessionTopic: "garage").isWordSearch)
        XCTAssertTrue(RetrievalSearchRequest(query: " roofer ").isWordSearch)
    }

    func testOversizedWordsTopicsAndBackwardsDatesAreRefusedBeforeSending() {
        let longWords = String(repeating: "a", count: RetrievalSearchRequest.maximumQueryBytes + 1)
        XCTAssertNotNil(RetrievalSearchRequest(query: longWords).problem(calendar: calendar))
        let longTopic = String(repeating: "t", count: RetrievalSearchRequest.maximumTopicBytes + 1)
        XCTAssertNotNil(RetrievalSearchRequest(sessionTopic: longTopic).problem(calendar: calendar))
        let backwards = RetrievalSearchRequest(since: day(2026, 10, 9), until: day(2026, 10, 8))
        XCTAssertNotNil(backwards.problem(calendar: calendar))
        let sameDay = RetrievalSearchRequest(since: day(2026, 10, 8, hour: 20), until: day(2026, 10, 8, hour: 1))
        XCTAssertNil(sameDay.problem(calendar: calendar), "a single day is a valid range whatever time the pickers hold")
    }

    func testFiltersCarryTheDayBoundsTopicAndTypeAndNameNoScope() {
        let request = RetrievalSearchRequest(
            query: "roofer", sessionTopic: " Garage ", onlyWithSessionTopic: true,
            since: day(2026, 10, 8, hour: 17), until: day(2026, 10, 9, hour: 3), itemType: .action, offset: 25)

        let filters = request.privateFilters(calendar: calendar)

        XCTAssertNil(filters.scopes, "the gate states the session's scopes; the screen never widens or names them")
        XCTAssertEqual(filters.itemTypes, ["action"])
        XCTAssertEqual(filters.sessionTopics, ["Garage"])
        XCTAssertTrue(filters.requireSessionTopic)
        XCTAssertEqual(filters.capturedAfter, "2026-10-08T00:00:00.000Z")
        XCTAssertEqual(filters.capturedBefore, "2026-10-09T23:59:59.999Z")
        XCTAssertEqual(filters.limit, RetrievalSearchRequest.pageSize)
        XCTAssertEqual(filters.offset, 25)
    }

    func testNoFilterLeavesTheDatesOutOfTheRequest() {
        let filters = RetrievalSearchRequest(query: "roofer").privateFilters(calendar: calendar)
        XCTAssertNil(filters.capturedAfter)
        XCTAssertNil(filters.capturedBefore)
        XCTAssertEqual(filters.itemTypes, [])
        XCTAssertEqual(filters.sessionTopics, [])
        XCTAssertFalse(filters.requireSessionTopic)
    }

    // MARK: references

    func testAnItemReferenceIsAValidatedLowercaseUUID() throws {
        let reference = try XCTUnwrap(RetrievalItemReference(candidate: Self.itemID.uppercased()))
        XCTAssertEqual(reference.value, Self.itemID)
        XCTAssertNil(RetrievalItemReference(candidate: ""))
        XCTAssertNil(RetrievalItemReference(candidate: "item-1"))
        XCTAssertNil(RetrievalItemReference(candidate: Self.itemID + "/../x"))
    }

    // MARK: failures

    func testCoreFailuresMapToCalmCategories() {
        func failure(_ errorClass: CoreErrorClass, _ code: String) -> RetrievalSearchFailure {
            RetrievalSearchFailure(core: CoreFailure(errorClass: errorClass, code: code, message: ""))
        }
        XCTAssertEqual(failure(.unauthorized, "read_not_authenticated"), .notAuthenticated)
        XCTAssertEqual(failure(.unauthorized, "read_relocked"), .relocked)
        XCTAssertEqual(failure(.permanent, "invalid_request"), .invalidRequest)
        XCTAssertEqual(failure(.transient, "busy"), .temporarilyUnavailable)
        XCTAssertEqual(failure(.permanent, "handle_closed"), .unavailable)
        XCTAssertEqual(failure(.unauthorized, "other"), .notAuthenticated)
    }

    // MARK: snippets

    func testASnippetSplitsTheTextAroundTheMatchByUnicodeScalar() {
        let snippet = RetrievalSnippet(text: "call the roofer today", span: CoreRetrievalMatchSpan(start: 9, end: 15))
        XCTAssertEqual(snippet, RetrievalSnippet(text: "call the roofer today", span: CoreRetrievalMatchSpan(start: 9, end: 15)))
        XCTAssertEqual(snippet.before, "call the ")
        XCTAssertEqual(snippet.match, "roofer")
        XCTAssertEqual(snippet.after, " today")
    }

    func testASnippetOffsetsCountScalarsNotCharacters() {
        let text = "e\u{301} roofer"
        let snippet = RetrievalSnippet(text: text, span: CoreRetrievalMatchSpan(start: 3, end: 9))
        XCTAssertEqual(snippet.match, "roofer")
        XCTAssertEqual(snippet.before, "e\u{301} ")
    }

    func testASnippetWithoutAUsableSpanShowsTheWholeTextUnmarked() {
        for span in [nil, CoreRetrievalMatchSpan(start: 4, end: 4), CoreRetrievalMatchSpan(start: 0, end: 99),
            CoreRetrievalMatchSpan(start: -1, end: 2)] {
            let snippet = RetrievalSnippet(text: "short text", span: span)
            XCTAssertEqual(snippet.before, "short text")
            XCTAssertEqual(snippet.match, "")
            XCTAssertEqual(snippet.after, "")
        }
    }

    func testALongLeadIsCroppedAroundTheMatch() {
        let lead = String(repeating: "x", count: 200)
        let snippet = RetrievalSnippet(text: lead + "roofer", span: CoreRetrievalMatchSpan(start: 200, end: 206))
        XCTAssertEqual(snippet.before.unicodeScalars.count, RetrievalSnippet.leadingContextScalars + 1)
        XCTAssertTrue(snippet.before.hasPrefix("…"))
        XCTAssertEqual(snippet.match, "roofer")
    }

    // MARK: pages

    func testAnAdmittedAnswerBecomesRowsWithBasisScopeDateAndReference() throws {
        let page = try RetrievalPage.make(
            from: try answer("""
                {"total_accessible": 3, "hits": [
                  {"item_id": "\(Self.itemID)", "capture_id": "c1", "scope": "work", "item_type": "note",
                   "text": "call the roofer", "text_basis": "corrected", "original_text": "call the rooter",
                   "matched": "superseded_original", "match_span": null,
                   "captured_at": "2026-10-08T12:30:00.123456789+00:00"},
                  {"item_id": "\(Self.itemID.replacingOccurrences(of: "0b6c", with: "1b6c"))", "capture_id": "c2",
                   "scope": "personal", "item_type": "broad_intention", "text": "plain words", "text_basis": "original", "original_text": "ignored",
                   "matched": "current", "match_span": {"start": 6, "end": 11},
                   "captured_at": "2026-10-09T08:00:00Z"}
                ]}
                """),
            for: RetrievalSearchRequest(query: "roofer", offset: 0))

        XCTAssertEqual(page.totalAccessible, 3)
        XCTAssertTrue(page.hasMore)
        XCTAssertEqual(page.rows.count, 2)
        let corrected = page.rows[0]
        XCTAssertEqual(corrected.reference.value, Self.itemID)
        XCTAssertEqual(corrected.text, "call the roofer")
        XCTAssertEqual(corrected.originalText, "call the rooter")
        XCTAssertEqual(corrected.basisTitle, "Corrected")
        XCTAssertEqual(corrected.matched, .supersededOriginal)
        XCTAssertEqual(corrected.snippet.match, "", "a replaced original that matched has no span in the current text")
        XCTAssertEqual(corrected.scopeName, "Work")
        XCTAssertEqual(corrected.itemType, .note)
        XCTAssertEqual(corrected.capturedAt.timeIntervalSince1970, 1_791_462_600.123, accuracy: 0.001)
        let plain = page.rows[1]
        XCTAssertNil(plain.originalText, "an original-basis hit shows no separate original")
        XCTAssertEqual(plain.snippet.match, "words")
        XCTAssertEqual(plain.basisTitle, "Original words")
        XCTAssertEqual(plain.scopeName, "Personal")
        XCTAssertEqual(plain.itemType, .broadIntention, "the type comes from the hit, not from a filter")
    }

    func testEachHitShowsItsOwnTypeWhateverTheRequestFilteredTo() throws {
        func answerJSON(itemTypeField: String) -> String {
            """
            {"total_accessible": 1, "hits": [{"item_id": "\(Self.itemID)", "capture_id": "c1", "scope": "personal",
            \(itemTypeField) "text": "t", "text_basis": "transcribed", "original_text": null, "matched": "current",
            "match_span": null, "captured_at": "2026-10-08T12:30:00Z"}]}
            """
        }
        let typed = try RetrievalPage.make(
            from: try answer(answerJSON(itemTypeField: #""item_type": "action","#)),
            for: RetrievalSearchRequest(query: "t", itemType: .idea))
        XCTAssertEqual(typed.rows[0].itemType, .action, "a filter is not echoed back as the type")
        XCTAssertEqual(typed.rows[0].typeTitle, "Action")
        XCTAssertEqual(typed.rows[0].basisTitle, "Transcribed")
        XCTAssertFalse(typed.hasMore)

        for field in [#""item_type": null,"#, ""] {
            let unclassified = try RetrievalPage.make(
                from: try answer(answerJSON(itemTypeField: field)), for: RetrievalSearchRequest(query: "t"))
            XCTAssertNil(unclassified.rows[0].itemType)
            XCTAssertEqual(unclassified.rows[0].typeTitle, "Not yet sorted", "an unclassified item says so")
        }
    }

    func testATypeLabelTheAppDoesNotKnowMakesThePageUnreadable() throws {
        let json = """
            {"total_accessible": 1, "hits": [{"item_id": "\(Self.itemID)", "capture_id": "c1", "scope": "personal",
            "item_type": "mystery", "text": "t", "text_basis": "original", "original_text": null, "matched": "current",
            "match_span": null, "captured_at": "2026-10-08T12:30:00Z"}]}
            """
        XCTAssertThrowsError(try RetrievalPage.make(from: try answer(json), for: RetrievalSearchRequest(query: "t"))) {
            XCTAssertEqual($0 as? RetrievalSearchFailure, .unreadable)
        }
    }

    func testAMalformedIdentifierOrDateMakesTheWholePageUnreadable() throws {
        func hit(_ itemID: String, _ capturedAt: String) -> String {
            """
            {"total_accessible": 1, "hits": [{"item_id": "\(itemID)", "capture_id": "c1", "scope": "personal",
            "text": "t", "text_basis": "original", "original_text": null, "matched": "current", "match_span": null,
            "captured_at": "\(capturedAt)"}]}
            """
        }
        for json in [hit("not-a-uuid", "2026-10-08T12:30:00Z"), hit(Self.itemID, "yesterday")] {
            XCTAssertThrowsError(
                try RetrievalPage.make(from: try answer(json), for: RetrievalSearchRequest(query: "t"))
            ) { error in
                XCTAssertEqual(error as? RetrievalSearchFailure, .unreadable)
            }
        }
    }

    func testAnEmptyAnswerIsAnEmptyPage() throws {
        let page = try RetrievalPage.make(
            from: try answer(#"{"total_accessible": 0, "hits": []}"#), for: RetrievalSearchRequest(query: "zebra"))
        XCTAssertTrue(page.rows.isEmpty)
        XCTAssertFalse(page.hasMore)
    }

    func testTheNextPageContinuesFromTheRequestedOffset() throws {
        let json = """
            {"total_accessible": 40, "hits": [{"item_id": "\(Self.itemID)", "capture_id": "c1", "scope": "personal",
            "text": "t", "text_basis": "original", "original_text": null, "matched": "current", "match_span": null,
            "captured_at": "2026-10-08T12:30:00Z"}]}
            """
        let last = try RetrievalPage.make(
            from: try answer(json), for: RetrievalSearchRequest(query: "t", offset: 39))
        XCTAssertFalse(last.hasMore)
        let middle = try RetrievalPage.make(
            from: try answer(json), for: RetrievalSearchRequest(query: "t", offset: 25))
        XCTAssertTrue(middle.hasMore)
    }
}
