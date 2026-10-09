import XCTest
@testable import OhAnd

/// Behavior of the capture field's state: honest results, durable-only clearing, and stable capture identity.
final class TextCaptureModelTests: XCTestCase {
    private var ingress: ScriptedTextCaptureIngress!
    private var model: TextCaptureModel!
    private var generatedIDs: Int!

    override func setUp() {
        super.setUp()
        ingress = ScriptedTextCaptureIngress()
        generatedIDs = 0
        model = TextCaptureModel(
            ingress: ingress,
            makeCaptureID: { [unowned self] in
                generatedIDs += 1
                return "capture-\(generatedIDs!)"
            },
            now: { Date(timeIntervalSince1970: 1_790_000_000) })
    }

    private func saved(_ captureID: String, already: Bool = false) -> TextCaptureOutcome {
        .saved(captureID: captureID, itemID: "item-1", alreadySaved: already)
    }

    func testSaveNeedsOnlyTextAndSubmitsItExactly() {
        let typed = "  call the dentist\n\u{1F9B7} tomorrow  \n"
        ingress.outcomes = [saved("capture-1")]
        model.updateText(typed)

        XCTAssertTrue(model.canSave)
        model.save()

        XCTAssertEqual(ingress.submissions.count, 1)
        XCTAssertEqual(ingress.submissions.first?.text, typed, "the original text is submitted without trimming")
        XCTAssertEqual(ingress.submissions.first?.captureID, "capture-1")
        XCTAssertEqual(ingress.submissions.first?.capturedAt, Date(timeIntervalSince1970: 1_790_000_000))
        XCTAssertEqual(model.status, .saved(alreadySaved: false))
    }

    func testFieldIsClearedOnlyAfterTheSaveIsConfirmed() {
        ingress.holdsCompletions = true
        ingress.outcomes = [saved("capture-1")]
        model.updateText("buy oat milk")

        model.save()

        XCTAssertEqual(model.status, .saving)
        XCTAssertEqual(model.text, "buy oat milk", "text stays while the save is unconfirmed")
        XCTAssertFalse(model.canSave)

        ingress.release()

        XCTAssertEqual(model.text, "")
        XCTAssertEqual(model.status, .saved(alreadySaved: false))
    }

    func testBlankTextIsNeverSubmitted() {
        XCTAssertFalse(model.canSave)
        model.save()
        model.updateText(" \n\t ")
        XCTAssertFalse(model.canSave)
        model.save()

        XCTAssertTrue(ingress.submissions.isEmpty)
        XCTAssertEqual(model.status, .idle)
    }

    func testRepeatedTapWhileSavingSubmitsOnce() {
        ingress.holdsCompletions = true
        ingress.outcomes = [saved("capture-1")]
        model.updateText("one note")

        model.save()
        model.save()
        model.save()
        ingress.release()

        XCTAssertEqual(ingress.submissions.count, 1)
        XCTAssertEqual(model.status, .saved(alreadySaved: false))
    }

    func testFailureToStageKeepsTheTextAndAnUnchangedRetryIsTheSameCapture() {
        ingress.outcomes = [.notStaged(captureID: "capture-1"), saved("capture-1")]
        model.updateText("renew passport")

        model.save()

        XCTAssertEqual(model.status, .notSaved)
        XCTAssertEqual(model.text, "renew passport")
        XCTAssertTrue(model.canSave, "the user can try again")

        model.save()

        XCTAssertEqual(ingress.submissions.map(\.captureID), ["capture-1", "capture-1"])
        XCTAssertEqual(ingress.submissions[0].capturedAt, ingress.submissions[1].capturedAt)
        XCTAssertEqual(model.status, .saved(alreadySaved: false))
        XCTAssertEqual(model.text, "")
    }

    func testEditingAfterAFailedSaveSubmitsTheEditedTextAsItsOwnCapture() {
        ingress.outcomes = [.notStaged(captureID: "capture-1"), saved("capture-2")]
        model.updateText("renew passport")
        model.save()

        model.updateText("renew passport in May")
        XCTAssertEqual(model.status, .idle, "a stale failure message does not outlive an edit")
        model.save()

        XCTAssertEqual(ingress.submissions.map(\.captureID), ["capture-1", "capture-2"])
        XCTAssertEqual(ingress.submissions[1].text, "renew passport in May")
    }

    func testKeptOnDeviceClearsTheFieldButIsNeverReportedAsSaved() {
        ingress.outcomes = [.keptOnDevice(captureID: "capture-1")]
        model.updateText("water the plants")

        model.save()

        XCTAssertEqual(model.status, .keptOnDevice)
        XCTAssertEqual(model.text, "", "the text is durable on the device, so the field takes the next capture")
        let message = model.status.message ?? ""
        XCTAssertFalse(message.hasPrefix("Saved"), message)
        XCTAssertTrue(message.contains("not confirmed saved"), message)
        XCTAssertTrue(message.contains("No reminder has been set"), message)
    }

    func testSavedMessageDoesNotImplyAReminderExists() {
        let message = TextCaptureModel.Status.saved(alreadySaved: false).message ?? ""

        XCTAssertTrue(message.hasPrefix("Saved"))
        XCTAssertTrue(message.contains("no reminder has been set"), message)
    }

    func testAlreadySavedConfirmationConvergesOnTheSameCapture() {
        ingress.outcomes = [saved("capture-1", already: true)]
        model.updateText("pay rent")

        model.save()

        XCTAssertEqual(model.status, .saved(alreadySaved: true))
        XCTAssertEqual(model.text, "")
    }

    func testDeletedItemIsNotReportedAsSaved() {
        ingress.outcomes = [.itemDeleted(captureID: "capture-1")]
        model.updateText("old note")

        model.save()

        XCTAssertEqual(model.status, .itemDeleted)
        XCTAssertNotEqual(model.status.message?.hasPrefix("Saved"), true)
    }

    func testTextTypedWhileASaveIsInFlightIsKeptAsTheNextCapture() {
        ingress.holdsCompletions = true
        ingress.outcomes = [saved("capture-1"), saved("capture-2")]
        model.updateText("first")
        model.save()

        model.updateText("first and second")
        ingress.release()

        XCTAssertEqual(model.text, "first and second")
        ingress.holdsCompletions = false
        model.save()
        XCTAssertEqual(ingress.submissions.map(\.captureID), ["capture-1", "capture-2"])
        XCTAssertEqual(ingress.submissions[1].text, "first and second")
    }

    func testEditingClearsAFinishedStatus() {
        ingress.outcomes = [saved("capture-1")]
        model.updateText("done")
        model.save()
        XCTAssertNotNil(model.status.message)

        model.updateText("next")

        XCTAssertEqual(model.status, .idle)
        XCTAssertNil(model.status.message)
    }
}
