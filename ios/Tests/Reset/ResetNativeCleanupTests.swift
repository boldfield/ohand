import XCTest
@testable import OhAndServices

/// The production native pieces of reset over real protected-storage files: the ingress staging drain and purge, the
/// export temporaries, and the durable markers.
final class ResetNativeCleanupTests: IngressStorageTestCase {
    private var stagingStore: IngressStagingStore {
        IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
    }

    private func makeIngress(
        importer: ForegroundIngressImporting,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) throws -> (NativeResetIngress, ForegroundIngressService) {
        let gate = ResetIngressGate(wrapping: importer, layout: layout, markers: FileResetMarkerStore(layout: layout))
        let service = try makeService(importer: gate)
        return (NativeResetIngress(service: service, gate: gate, layout: layout, fileSystem: fileSystem), service)
    }

    private func filesMentioning(_ captureID: String) -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: layout.rootDirectory.path)) ?? []
        return subpaths.filter { ($0 as NSString).lastPathComponent.contains(captureID) }.sorted()
    }

    // MARK: Ingress staging

    func testSnapshotNamesStagedRecordsAndInterruptedWrites() async throws {
        let (ingress, service) = try makeIngress(importer: FakeForegroundIngressImporter())
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        try Data("{\"text\":\"buy oat".utf8).write(to: recordURL("cap-b").appendingPathExtension("incomplete"))

        let snapshot = try await ingress.snapshot()
        ingress.endBoundary()

        XCTAssertEqual(snapshot.captureIDs, ["cap-a", "cap-b"])
    }

    func testPurgeRemovesTheSnapshotRecordsAndTheirAudioButNotInputsStagedAfterIt() async throws {
        let (ingress, service) = try makeIngress(importer: FakeForegroundIngressImporter())
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(try audioRecord("cap-a"))
        _ = try stagingStore.stage(textRecord("cap-b"))
        let snapshot = try await ingress.snapshot()
        _ = try stagingStore.stage(textRecord("cap-late"))

        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(purge, .purged(count: 2))
        XCTAssertEqual(filesMentioning("cap-a"), [])
        XCTAssertEqual(filesMentioning("cap-b"), [])
        XCTAssertTrue(exists(recordURL("cap-late")), "a capture staged after the tap is fresh-generation content")
    }

    func testImportedInputsLeaveNothingToPurgeAndUnimportedOnesAreRemoved() async throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a")]
        let (ingress, service) = try makeIngress(importer: importer)
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        _ = try stagingStore.stage(textRecord("cap-b"))
        let snapshot = try await ingress.snapshot()

        await ingress.importSnapshot(snapshot)
        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(Set(importer.importedRecords.map { $0.captureID }), ["cap-a", "cap-b"])
        XCTAssertEqual(purge, .purged(count: 1), "cap-a imported and its record was removed; cap-b was unimported")
        XCTAssertEqual(filesMentioning("cap-a"), [])
        XCTAssertEqual(filesMentioning("cap-b"), [])
    }

    func testOnlyTheSnapshotIsImportedAndAValidRecordStagedAfterItStaysForTheFreshGeneration() async throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [
            FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a"),
            FakeForegroundIngressImporter.confirmation("cap-late", itemID: "item-late"),
        ]
        let (ingress, service) = try makeIngress(importer: importer)
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        let snapshot = try await ingress.snapshot()
        _ = try stagingStore.stage(textRecord("cap-late"))

        await ingress.importSnapshot(snapshot)
        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, ["cap-a"], "a record staged after the snapshot is never imported by reset")
        XCTAssertEqual(purge, .purged(count: 0), "the only snapshot record was imported and its staging record removed")
        XCTAssertTrue(exists(recordURL("cap-late")), "the later capture is still staged, valid and unimported")
        XCTAssertEqual(try stagingStore.load(captureID: "cap-late"), textRecord("cap-late"))
    }

    func testAnInputThatCannotBeRemovedIsReportedAndRetried() async throws {
        let faults = FaultPlan()
        let (ingress, service) = try makeIngress(
            importer: FakeForegroundIngressImporter(), fileSystem: FaultingDeletionFileSystem(faults: faults))
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        let snapshot = try await ingress.snapshot()
        faults.arm("fs.remove:cap-a.json")

        let first = await ingress.purgeUnimported(from: snapshot)
        XCTAssertEqual(first, .incomplete(remaining: 1))
        XCTAssertTrue(exists(recordURL("cap-a")), "a record that could not be removed stays for the retry")

        let retried = await ingress.purgeUnimported(from: snapshot)
        XCTAssertEqual(retried, .purged(count: 1))
        XCTAssertEqual(filesMentioning("cap-a"), [])
    }

    /// One process that lists `cap-a` at the tap and purges it once with the bound `cap-a.wav` refusing removal. The
    /// process's ingress (and its single-writer lock) is gone when this returns.
    private func purgeOnceWithAStuckBoundAudioFile() async throws -> (ResetStagingSnapshot, ResetStagingPurge) {
        let faults = FaultPlan()
        let (ingress, service) = try makeIngress(
            importer: FakeForegroundIngressImporter(), fileSystem: FaultingDeletionFileSystem(faults: faults))
        withExtendedLifetime(service) {}
        let snapshot = try await ingress.snapshot()
        ingress.endBoundary()
        faults.arm("fs.remove:cap-a.wav")
        return (snapshot, await ingress.purgeUnimported(from: snapshot))
    }

    func testTheRecordOutlivesEveryAudioFileSoAFailedPurgeIsRetriedAfterARelaunch() async throws {
        _ = try stagingStore.stage(try audioRecord("cap-a"))
        // Finalized audio bound to the capture that its record does not name, as a finalize interrupted after the copy.
        try Data("synthetic audio".utf8).write(to: finalizedAudioURL("cap-a.m4a"))
        try Data("synthetic audio".utf8).write(to: finalizedAudioURL("cap-a.wav"))

        let (snapshot, first) = try await purgeOnceWithAStuckBoundAudioFile()
        XCTAssertEqual(first, .incomplete(remaining: 1))
        XCTAssertTrue(exists(finalizedAudioURL("cap-a.wav")))
        XCTAssertTrue(exists(recordURL("cap-a")), "the record stays while any audio of the capture remains")

        // The relaunched process knows the capture only from the recorded list and what is still staged.
        let (relaunched, relaunchedService) = try makeIngress(importer: FakeForegroundIngressImporter())
        withExtendedLifetime(relaunchedService) {}
        let retried = await relaunched.purgeUnimported(from: snapshot)
        XCTAssertEqual(retried, .purged(count: 1))
        XCTAssertEqual(filesMentioning("cap-a"), [], "no audio survives the record that named it")
    }

    func testAnUnreadableStagingAreaIsUnknownNeverEmpty() async throws {
        let faults = FaultPlan()
        let (ingress, service) = try makeIngress(
            importer: FakeForegroundIngressImporter(), fileSystem: FaultingDeletionFileSystem(faults: faults))
        withExtendedLifetime(service) {}
        faults.arm("fs.entryNames")
        do {
            _ = try await ingress.snapshot()
            XCTFail("an unreadable staging area must not produce a snapshot")
        } catch {}

        faults.arm("fs.entryNames")
        let purge = await ingress.purgeUnimported(from: ResetStagingSnapshot(captureIDs: ["cap-a"]))
        XCTAssertEqual(purge, .unknown)
    }

    // MARK: Export temporaries

    private var exportsDirectory: URL {
        layout.directory(for: .temporaryFiles).appendingPathComponent(ExportService.exportsDirectoryName, isDirectory: true)
    }

    func testExportTemporariesAreRemovedAndAnAbsentDirectoryIsClean() throws {
        let cleaner = ExportTemporaryCleaner(layout: layout)
        XCTAssertTrue(cleaner.removeAll(), "nothing to remove is success")

        let nested = exportsDirectory.appendingPathComponent("export-1", isDirectory: true)
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        try Data("synthetic export".utf8).write(to: nested.appendingPathComponent("items.csv"))
        try Data("synthetic export".utf8).write(to: exportsDirectory.appendingPathComponent("loose.json"))
        let sibling = layout.directory(for: .temporaryFiles).appendingPathComponent("keep-me")
        try Data("not an export".utf8).write(to: sibling)

        XCTAssertTrue(cleaner.removeAll())
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: exportsDirectory.path), [])
        XCTAssertTrue(exists(sibling), "only the exports directory is touched")
    }

    // MARK: Markers

    private static let secretA = ResetBoundSecret(
        credentialReference: "ref-a",
        identity: ResetSecretIdentity(
            createdAtBits: Date(timeIntervalSinceReferenceDate: 812_345_678.123_456).timeIntervalSinceReferenceDate.bitPattern,
            modifiedAtBits: Date(timeIntervalSinceReferenceDate: 812_345_679.654_321).timeIntervalSinceReferenceDate.bitPattern))

    private static let boundProfiles = ResetCredentialRemoval(
        profiles: [
            ResetBoundProfile(profileVersion: "profile-a-v1", credentialReference: "ref-a"),
            ResetBoundProfile(profileVersion: "profile-b-v1", credentialReference: nil),
        ],
        secrets: [secretA])

    /// A tap; a `remove` tap binds `boundProfiles`.
    private func tap(_ choice: ResetCredentialChoice, _ captureIDs: Set<String>) -> ResetRequest {
        ResetRequest(
            credentialChoice: choice, credentialRemoval: choice == .remove ? Self.boundProfiles : nil,
            stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: captureIDs))
    }

    private var requestFileNames: [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: layout.directory(for: .temporaryFiles).path)) ?? []
        return names.filter { $0.hasPrefix(FileResetMarkerStore.requestFilePrefix) }.sorted()
    }

    func testTheRecordedTapIsDurableReplaceableAndIndependentOfTheOtherMarkers() throws {
        let store = FileResetMarkerStore(layout: layout)
        XCTAssertNil(try store.request())

        try store.recordRequest(tap(.remove, ["cap-a", "cap-b"]))
        let relaunched = FileResetMarkerStore(layout: layout)
        XCTAssertEqual(
            try relaunched.request(), tap(.remove, ["cap-a", "cap-b"]),
            "the tap, the profiles its choice binds and its list survive a restart")
        XCTAssertEqual(try relaunched.request()?.credentialRemoval, Self.boundProfiles)
        XCTAssertEqual(
            try relaunched.request()?.credentialRemoval?.secrets.first?.identity.modifiedAt, Self.secretA.identity.modifiedAt,
            "the identity of a bound secret survives a restart to the exact instant")
        XCTAssertFalse(try relaunched.isRemovalRequested(), "the choice is not armed by the tap alone")
        XCTAssertFalse(try relaunched.isCleanupOutstanding(), "cleanup is not outstanding for a reset that has not started")

        try store.recordRequest(tap(.keep, ["cap-c"]))
        XCTAssertEqual(try relaunched.request(), tap(.keep, ["cap-c"]), "the latest tap replaces the earlier one")
        XCTAssertEqual(requestFileNames.count, 1, "the earlier tap is removed once the new one is durable")

        try store.recordRequest(tap(.keep, []))
        XCTAssertEqual(try relaunched.request(), tap(.keep, []), "a repeated choice still records its own list")
        XCTAssertNil(try relaunched.request()?.credentialRemoval, "a keep tap binds no profile")

        try store.recordRequest(ResetRequest(
            credentialChoice: .remove, credentialRemoval: ResetCredentialRemoval(profiles: []),
            stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: [])))
        XCTAssertEqual(
            try relaunched.request()?.credentialRemoval, ResetCredentialRemoval(profiles: []),
            "a remove tap made with no profile configured binds none, and is still a remove tap")

        try store.clearRequest()
        try store.clearRequest()
        XCTAssertNil(try relaunched.request())
        XCTAssertEqual(requestFileNames, [])
    }

    func testTheRecordedTapKeepsTheCoreGenerationItSawAndReadsNoneAsNone() throws {
        let store = FileResetMarkerStore(layout: layout)
        let relaunched = FileResetMarkerStore(layout: layout)

        try store.recordRequest(ResetRequest(
            credentialChoice: .remove, credentialRemoval: Self.boundProfiles,
            stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: ["cap-a"]), awaitsEarlierReset: true,
            latestGenerationAtTheTap: 3))
        let afterThree = try XCTUnwrap(try relaunched.request())
        XCTAssertEqual(afterThree.latestGenerationAtTheTap, 3, "the core's generation at the tap survives a restart")
        XCTAssertTrue(afterThree.awaitsEarlierReset)
        XCTAssertFalse(afterThree.coreHasStarted(latest: progress(generation: 3)), "the generation at the tap is not the tap's own")
        XCTAssertTrue(afterThree.coreHasStarted(latest: progress(generation: 4)), "a newer generation is the tap's own")
        XCTAssertFalse(afterThree.coreHasStarted(latest: nil))

        try store.recordRequest(tap(.keep, []))
        let beforeAny = try XCTUnwrap(try relaunched.request())
        XCTAssertNil(beforeAny.latestGenerationAtTheTap, "a tap made before any reset notes no generation")
        XCTAssertFalse(beforeAny.coreHasStarted(latest: nil))
        XCTAssertTrue(beforeAny.coreHasStarted(latest: progress(generation: 1)), "any generation is newer than none")
    }

    private func progress(generation: Int64) -> ResetProgress {
        ResetProgress(
            generation: generation, phase: .completed, boundaryRowID: 0, itemsAwaitingTombstone: 0, cleanupPending: 0,
            cleanupFailed: 0)
    }

    func testReplacingATapNeverLosesItNorRevertsToTheEarlierChoice() throws {
        let faults = FaultPlan()
        let store = FileResetMarkerStore(layout: layout, fileSystem: FaultingResetMarkerFileSystem(faults: faults))
        let relaunched = FileResetMarkerStore(layout: layout)
        for (earlierChoice, laterChoice) in [(ResetCredentialChoice.remove, ResetCredentialChoice.keep), (.keep, .remove)] {
            let earlier = tap(earlierChoice, ["cap-early"])
            let later = tap(laterChoice, ["cap-early", "cap-late"])
            let context = "\(earlierChoice) replaced by \(laterChoice)"

            // Killed after the new tap was written and before the earlier one was removed.
            try store.recordRequest(earlier)
            faults.arm("fs.remove:\(FileResetMarkerStore.requestFilePrefix)1")
            try store.recordRequest(later)
            XCTAssertEqual(requestFileNames.count, 2, context)
            XCTAssertEqual(try relaunched.request(), later, "the newest complete tap is the request: \(context)")
            try store.clearRequest()
            XCTAssertNil(try relaunched.request(), "a clear consumes both files: \(context)")

            // The new tap cannot be written: the earlier tap is still recorded, untouched.
            try store.recordRequest(earlier)
            faults.arm("fs.write:\(FileResetMarkerStore.requestFilePrefix)2")
            XCTAssertThrowsError(try store.recordRequest(later), context)
            XCTAssertEqual(try relaunched.request(), earlier, "a failed replacement never loses the earlier tap: \(context)")
            try store.clearRequest()

            // A clear that fails part-way removes the older file first, so what remains is still the latest tap.
            try store.recordRequest(earlier)
            faults.arm("fs.remove:\(FileResetMarkerStore.requestFilePrefix)1")
            try store.recordRequest(later)
            faults.arm("fs.remove:\(FileResetMarkerStore.requestFilePrefix)2")
            XCTAssertThrowsError(try store.clearRequest(), context)
            XCTAssertEqual(try relaunched.request(), later, "a failed clear never exposes the earlier choice: \(context)")
            try store.clearRequest()
            XCTAssertNil(try relaunched.request(), context)
        }
    }

    func testAnInterruptedOrUnreadableRequestWriteIsNeverReadAsNoRequest() throws {
        let store = FileResetMarkerStore(layout: layout)
        let directory = layout.directory(for: .temporaryFiles)
        try Data("{".utf8).write(
            to: directory.appendingPathComponent(FileResetMarkerStore.requestFilePrefix + "1" + IngressFileSystemNaming.incompleteWriteSuffix))
        XCTAssertNil(try store.request(), "a write interrupted before it became a file is not a tap")

        try store.recordRequest(tap(.keep, ["cap-a"]))
        XCTAssertEqual(try store.request(), tap(.keep, ["cap-a"]))

        try Data("not a request".utf8).write(to: directory.appendingPathComponent(FileResetMarkerStore.requestFilePrefix + "9"))
        XCTAssertThrowsError(try store.request(), "a damaged request is unreadable, never taken as no request")

        try store.clearRequest()
        try Data(#"{"credentialChoice":"remove","stagedBeforeTheTap":[]}"#.utf8)
            .write(to: directory.appendingPathComponent(FileResetMarkerStore.requestFilePrefix + "1"))
        XCTAssertThrowsError(
            try store.request(), "a remove tap that does not say which profiles it binds is unreadable, never unbounded")

        try store.clearRequest()
        try Data(#"{"credentialChoice":"remove","profilesToRemove":[],"stagedBeforeTheTap":[]}"#.utf8)
            .write(to: directory.appendingPathComponent(FileResetMarkerStore.requestFilePrefix + "1"))
        XCTAssertThrowsError(
            try store.request(), "a remove tap that does not say which secrets it binds is unreadable, never every secret")
    }

    func testTheRemovalMarkerKeepsWhatTheChoiceBindsAndIsNeverReadAsUnbounded() throws {
        let store = FileResetMarkerStore(layout: layout)
        let directory = layout.directory(for: .temporaryFiles)
        XCTAssertNil(try store.removalRequest())

        try store.recordRemovalRequested(Self.boundProfiles)
        let relaunched = FileResetMarkerStore(layout: layout)
        XCTAssertEqual(try relaunched.removalRequest(), Self.boundProfiles, "the bound profiles and secrets survive a restart")

        let widened = ResetCredentialRemoval(
            profiles: Self.boundProfiles.profiles + [
                ResetBoundProfile(profileVersion: "profile-c-v1", credentialReference: "ref-c"),
            ],
            secrets: Self.boundProfiles.secrets + [
                ResetBoundSecret(credentialReference: "ref-c", identity: ResetSecretIdentity(createdAtBits: 7, modifiedAtBits: 9)),
            ])
        try store.recordRemovalRequested(widened)
        XCTAssertEqual(
            try relaunched.removalRequest(), Self.boundProfiles,
            "an owed removal is never widened while it is outstanding")

        try store.clearRemovalRequest()
        XCTAssertNil(try relaunched.removalRequest())
        try store.recordRemovalRequested(widened)
        XCTAssertEqual(try relaunched.removalRequest(), widened, "a removal recorded after the earlier one was spent binds its own profiles")

        try store.clearRemovalRequest()
        try Data("1".utf8).write(to: directory.appendingPathComponent(FileResetMarkerStore.removalFileName))
        XCTAssertThrowsError(
            try relaunched.removalRequest(), "a marker that does not say what it binds is unreadable, never every profile")
        XCTAssertThrowsError(try relaunched.isRemovalRequested())

        try Data(#"{"profiles":[{"profileVersion":"profile-a-v1","credentialReference":"ref-a"}]}"#.utf8)
            .write(to: directory.appendingPathComponent(FileResetMarkerStore.removalFileName))
        XCTAssertThrowsError(
            try relaunched.removalRequest(),
            "a marker that names references without the identity of their secrets is unreadable, never whatever is stored now")
    }

    func testMarkersAreDurableIdempotentAndIndependent() throws {
        let store = FileResetMarkerStore(layout: layout)
        XCTAssertFalse(try store.isRemovalRequested())
        XCTAssertFalse(try store.isCleanupOutstanding())

        try store.recordRemovalRequested(Self.boundProfiles)
        try store.recordRemovalRequested(Self.boundProfiles)
        let relaunched = FileResetMarkerStore(layout: layout)
        XCTAssertTrue(try relaunched.isRemovalRequested(), "the choice survives a restart")
        XCTAssertFalse(try relaunched.isCleanupOutstanding())

        try store.recordCleanupOutstanding()
        try store.clearRemovalRequest()
        try store.clearRemovalRequest()
        XCTAssertFalse(try relaunched.isRemovalRequested())
        XCTAssertTrue(try relaunched.isCleanupOutstanding())

        try store.clearCleanupOutstanding()
        XCTAssertFalse(try relaunched.isCleanupOutstanding())
    }
}
