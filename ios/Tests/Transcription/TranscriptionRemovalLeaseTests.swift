import Foundation
import XCTest
@testable import OhAndServices

/// The removal lease a transcription job takes in the playback fence: a deletion ends it wherever it lands, including
/// between the admission and the publication of the lease, and a deletion since the job read the generation keeps it
/// from being taken at all.
final class TranscriptionRemovalLeaseTests: XCTestCase {
    private let itemID = "item-1"
    private var directory: URL!
    private var audioURL: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-removal-lease-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        audioURL = directory.appendingPathComponent("clip-1.m4a")
        try Data([0x01, 0x02, 0x03]).write(to: audioURL)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private func removalRuns(_ fence: PlaybackFence) -> Bool {
        var ran = false
        do {
            try fence.withUnheld(audioURL) { ran = true }
        } catch {
            return false
        }
        return ran
    }

    func testTheLeaseKeepsRemovalOutUntilItIsReleased() throws {
        let fence = PlaybackFence()

        let release = try XCTUnwrap(
            fence.holdAgainstRemoval(itemID: itemID, at: audioURL, generation: fence.removalGeneration(of: itemID)))

        XCTAssertTrue(fence.isHeld(audioURL))
        XCTAssertFalse(removalRuns(fence))
        release()
        XCTAssertFalse(fence.isHeld(audioURL))
        XCTAssertTrue(removalRuns(fence))
    }

    func testADeletionBetweenAdmissionAndPublicationStillEndsTheLease() {
        let fence = PlaybackFence()
        let generation = fence.removalGeneration(of: itemID)
        var heldWhenDeleted = false

        let release = fence.holdAgainstRemoval(
            itemID: itemID, at: audioURL, generation: generation,
            afterAdmission: {
                // The lease is admitted but its identifier is not published yet when the deletion arrives.
                heldWhenDeleted = fence.isHeld(audioURL)
                fence.endPlayback(itemID: itemID)
            })

        XCTAssertTrue(heldWhenDeleted, "the deletion landed inside the window")
        XCTAssertNil(release, "a lease ended by a deletion is not handed to the job")
        XCTAssertFalse(fence.isHeld(audioURL))
        XCTAssertTrue(removalRuns(fence), "the deletion's removal is not kept waiting")
    }

    func testADeletionAfterPublicationEndsTheLeaseAndALaterReleaseIsHarmless() throws {
        let fence = PlaybackFence()
        let release = try XCTUnwrap(
            fence.holdAgainstRemoval(itemID: itemID, at: audioURL, generation: fence.removalGeneration(of: itemID)))

        fence.endPlayback(itemID: itemID)

        XCTAssertFalse(fence.isHeld(audioURL))
        // A playback lease taken after the deletion is not released by the job's own release.
        let playback = try fence.admit(
            itemID: "item-2", url: audioURL, generation: fence.generation(of: "item-2"), verify: {}, end: { _ in })
        release()
        XCTAssertTrue(fence.isHeld(audioURL))
        fence.release(playback)
        XCTAssertTrue(removalRuns(fence))
    }

    func testADeletionSinceTheGenerationWasReadRefusesTheLease() {
        let fence = PlaybackFence()
        let generation = fence.removalGeneration(of: itemID)

        fence.endPlayback(itemID: itemID)

        XCTAssertNil(fence.holdAgainstRemoval(itemID: itemID, at: audioURL, generation: generation))
        XCTAssertFalse(fence.isHeld(audioURL))
    }

    func testAnAbsentFileTakesNoLease() throws {
        let fence = PlaybackFence()
        try FileManager.default.removeItem(at: audioURL)

        XCTAssertNil(
            fence.holdAgainstRemoval(itemID: itemID, at: audioURL, generation: fence.removalGeneration(of: itemID)))
        XCTAssertFalse(fence.isHeld(audioURL))
    }

    func testDeletionsRacingAdmissionNeverLeaveTheFenceHeld() {
        let fence = PlaybackFence()
        let url: URL = audioURL
        let itemID = self.itemID
        DispatchQueue.concurrentPerform(iterations: 200) { iteration in
            if iteration.isMultiple(of: 2) {
                let release = fence.holdAgainstRemoval(
                    itemID: itemID, at: url, generation: fence.removalGeneration(of: itemID))
                release?()
            } else {
                fence.endPlayback(itemID: itemID)
            }
        }
        XCTAssertFalse(fence.isHeld(url))
    }
}
