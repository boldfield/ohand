import Foundation

enum DeletionLocalCleanupError: Error, Equatable {
    /// The capture identifier could name a path, so no file operation is attempted for it.
    case unsafeCaptureIdentifier
    /// The core's audio reference does not name a file inside the finalized audio store that is bound to the capture.
    /// The referenced file is left alone, and every file bound to the capture was still removed first.
    case audioReferenceUnresolvable
    /// A removal reported success but the file is still present.
    case stillPresent
    /// Another staging record that cannot be trusted names audio this capture's record names. Nothing that record
    /// could own was removed, and this capture's record stays so it still names the audio left to remove.
    case ingressOwnershipUnresolved
}

/// Removes the files a deleted item's capture left in protected storage: the ingress staging record and the audio it
/// owns, and the finalized audio. Every operation is idempotent and verifies absence afterwards, so a repeat after an
/// interruption converges instead of failing on what an earlier attempt already removed.
///
/// A finalized audio name is bound to its capture (`<captureID>.<extension>`) and capture identifiers contain no dots,
/// so a prefix match can never touch another capture's file. A name a staging record gives is removed only when the
/// deleted capture owns it, so a damaged or tampered record can never point cleanup at another capture's audio.
final class DeletionLocalCleanup {
    private let layout: ProtectedStorageLayout
    private let fileSystem: IngressFileSystem
    private let staging: IngressStagingStore

    init(layout: ProtectedStorageLayout, fileSystem: IngressFileSystem = FileManagerIngressFileSystem()) {
        self.layout = layout
        self.fileSystem = fileSystem
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
    }

    /// Removes the staging record, an interrupted write of it, and every audio file the capture owns in the in-progress
    /// and finalized stores. Audio goes first and the record last: a record that survives an interruption still names
    /// the audio left to remove.
    ///
    /// The audio names come from three sources, so a record that cannot be parsed never hides audio: the hand-off
    /// names in the record, the names salvaged from a corrupt or unsupported record, and the names the voice recorder
    /// binds to the capture ID (`<captureID>.<extension>`, `<captureID>-continued.wav`, `<captureID>-joined.wav`).
    /// Names from the record are kept only when the capture owns them (see `ownsInProgressName` and
    /// `ownsFinalizedName`); a name it cannot be shown to own is left in place for its owner.
    func clearIngress(captureID: String) throws {
        guard IngressRecord.isSafeCaptureID(captureID) else { throw DeletionLocalCleanupError.unsafeCaptureIdentifier }

        let recordURL = staging.recordURL(captureID: captureID)
        var inProgressNames = Set(try boundInProgressNames(captureID: captureID))
        var finalizedNames = Set<String>()
        if let handoff = try recordedAudio(captureID: captureID, recordURL: recordURL) {
            if try ownsInProgressName(handoff.inProgressFileName, captureID: captureID, recordURL: recordURL) {
                inProgressNames.insert(handoff.inProgressFileName)
            }
            if Self.ownsFinalizedName(handoff.finalizedFileName, captureID: captureID) {
                finalizedNames.insert(handoff.finalizedFileName)
            }
        }
        for name in inProgressNames.sorted() {
            try removeIfPresent(staging.inProgressAudioURL(fileName: name))
        }
        for name in finalizedNames.sorted() {
            try removeIfPresent(staging.finalizedAudioURL(fileName: name))
        }
        try removeIfPresent(URL(fileURLWithPath: recordURL.path + IngressFileSystemNaming.incompleteWriteSuffix))
        try removeIfPresent(recordURL)
    }

    /// Removes the finalized audio the core references and every finalized file bound to the capture. The reference is
    /// trusted only as far as the capture owns the file it names (see `ownsFinalizedName`): a reference to another
    /// capture's audio removes nothing of that capture's and is reported unresolvable.
    func removeAudio(captureID: String, audioReference: String?) throws {
        guard IngressRecord.isSafeCaptureID(captureID) else { throw DeletionLocalCleanupError.unsafeCaptureIdentifier }

        var urls: [URL] = []
        var referenceResolvable = true
        if let audioReference {
            if let referenced = staging.finalizedAudioURL(forReference: audioReference),
               Self.ownsFinalizedName(referenced.lastPathComponent, captureID: captureID) {
                urls.append(referenced)
            } else {
                referenceResolvable = false
            }
        }
        let boundPrefix = captureID + "."
        let finalizedDirectory = layout.directory(for: .finalizedAudio)
        for name in try fileSystem.entryNames(in: finalizedDirectory).sorted() where name.hasPrefix(boundPrefix) {
            urls.append(finalizedDirectory.appendingPathComponent(name))
        }
        for url in urls {
            try removeIfPresent(url)
        }
        guard referenceResolvable else { throw DeletionLocalCleanupError.audioReferenceUnresolvable }
    }

    /// The audio hand-off the staging record names, or nil when there is no record or it names none. A record that
    /// exists but cannot be read now (for example while the device is locked) throws, because treating it as absent
    /// would leave its audio behind. A record that is corrupt, unsupported or invalid is parsed leniently for the same
    /// two names. The record's file name ties it to the deleted capture, but its body does not prove the names it
    /// gives belong to that capture, so the caller checks ownership of each before removing anything.
    private func recordedAudio(captureID: String, recordURL: URL) throws -> IngressAudioHandoff? {
        do {
            return try staging.load(captureID: captureID)?.audio
        } catch IngressStagingFailure.corruptRecord {
        } catch IngressStagingFailure.unsupportedRecordVersion {
        } catch IngressStagingFailure.invalidRecord {
        }
        return try salvagedAudio(from: recordURL)
    }

    private func salvagedAudio(from recordURL: URL) throws -> IngressAudioHandoff? {
        let data = try fileSystem.read(from: recordURL)
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let audio = object["audio"] as? [String: Any],
              let inProgress = audio["inProgressFileName"] as? String,
              let finalized = audio["finalizedFileName"] as? String,
              IngressRecord.isSafeFileName(inProgress), IngressRecord.isSafeFileName(finalized) else { return nil }
        return IngressAudioHandoff(inProgressFileName: inProgress, finalizedFileName: finalized)
    }

    /// A finalized name is the capture's only when it is bound to the deletion target's capture ID, whatever the
    /// record body or the core's audio reference claims. Any other name is left alone; the capture's own finalized
    /// audio is still found by its bound name.
    private static func ownsFinalizedName(_ name: String, captureID: String) -> Bool {
        name.hasPrefix(captureID + ".")
    }

    /// An in-progress name the record gives is the capture's when the voice recorder binds it to the capture ID, or
    /// when nothing else can claim it. The recorder names every recording `<id>.wav`, `<id>-continued.wav` or
    /// `<id>-joined.wav`, and voice recovery attributes a `.wav` file to the capture its name binds, so a `.wav` name
    /// bound to another ID belongs to that capture. Any other name is claimed only through a staging record, so it
    /// is the capture's when no other record or interrupted record write names it. A record that cannot be read now
    /// throws, because its claim is unknown.
    private func ownsInProgressName(_ name: String, captureID: String, recordURL: URL) throws -> Bool {
        if Self.isBoundInProgressName(name, captureID: captureID) { return true }
        guard !name.hasSuffix(".wav") else { return false }
        return try !isClaimedByAnotherRecord(inProgressName: name, recordURL: recordURL)
    }

    /// True when another record that loads and validates names the file: as in ingress recovery, only such a record
    /// claims audio, so the file stays that capture's and is never surfaced as unclaimed. Another entry that names the
    /// file but cannot be trusted (a corrupt, unsupported or invalid record, or an interrupted record write) neither
    /// proves nor rules out a claim, so it throws `ingressOwnershipUnresolved` and the step stays pending.
    private func isClaimedByAnotherRecord(inProgressName name: String, recordURL: URL) throws -> Bool {
        let ownNames: Set<String> = [
            recordURL.lastPathComponent, recordURL.lastPathComponent + IngressFileSystemNaming.incompleteWriteSuffix,
        ]
        let recordSuffix = "." + IngressRecord.fileExtension
        let recordsDirectory = layout.directory(for: .ingressStagingRecords)
        var untrustedClaim = false
        for entry in try fileSystem.entryNames(in: recordsDirectory).sorted()
        where !ownNames.contains(entry) && !entry.hasPrefix(".") {
            if entry.hasSuffix(recordSuffix) {
                do {
                    let record = try staging.load(captureID: String(entry.dropLast(recordSuffix.count)))
                    if record?.audio?.inProgressFileName == name { return true }
                    continue
                } catch IngressStagingFailure.corruptRecord {
                } catch IngressStagingFailure.unsupportedRecordVersion {
                } catch IngressStagingFailure.invalidRecord {
                }
            }
            let data = try fileSystem.read(from: recordsDirectory.appendingPathComponent(entry))
            if Self.inProgressName(in: data) == name { untrustedClaim = true }
        }
        guard !untrustedClaim else { throw DeletionLocalCleanupError.ingressOwnershipUnresolved }
        return false
    }

    private static func inProgressName(in data: Data) -> String? {
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let audio = object["audio"] as? [String: Any] else { return nil }
        return audio["inProgressFileName"] as? String
    }

    private static func isBoundInProgressName(_ name: String, captureID: String) -> Bool {
        name.hasPrefix(captureID + ".") || name == captureID + "-continued.wav" || name == captureID + "-joined.wav"
    }

    private func boundInProgressNames(captureID: String) throws -> [String] {
        try fileSystem.entryNames(in: layout.directory(for: .ingressInProgressAudio))
            .filter { Self.isBoundInProgressName($0, captureID: captureID) }
            .sorted()
    }

    private func removeIfPresent(_ url: URL) throws {
        if fileSystem.fileExists(at: url) {
            try fileSystem.remove(at: url)
        }
        guard !fileSystem.fileExists(at: url) else { throw DeletionLocalCleanupError.stillPresent }
    }
}
