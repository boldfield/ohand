import Foundation

enum DeletionLocalCleanupError: Error, Equatable {
    /// The capture identifier could name a path, so no file operation is attempted for it.
    case unsafeCaptureIdentifier
    /// The core's audio reference does not name a file inside the finalized audio store. Every file bound to the
    /// capture was still removed first.
    case audioReferenceUnresolvable
    /// A removal reported success but the file is still present.
    case stillPresent
}

/// Removes the files a deleted item's capture left in protected storage: the ingress staging record and the audio it
/// owns, and the finalized audio. Every operation is idempotent and verifies absence afterwards, so a repeat after an
/// interruption converges instead of failing on what an earlier attempt already removed.
///
/// A finalized audio name is bound to its capture (`<captureID>.<extension>`) and capture identifiers contain no dots,
/// so a prefix match can never touch another capture's file.
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
    func clearIngress(captureID: String) throws {
        guard IngressRecord.isSafeCaptureID(captureID) else { throw DeletionLocalCleanupError.unsafeCaptureIdentifier }

        let recordURL = staging.recordURL(captureID: captureID)
        var inProgressNames = Set(try boundInProgressNames(captureID: captureID))
        var finalizedNames = Set<String>()
        if let handoff = try recordedAudio(captureID: captureID, recordURL: recordURL) {
            inProgressNames.insert(handoff.inProgressFileName)
            finalizedNames.insert(handoff.finalizedFileName)
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

    /// Removes the finalized audio the core references and every finalized file bound to the capture.
    func removeAudio(captureID: String, audioReference: String?) throws {
        guard IngressRecord.isSafeCaptureID(captureID) else { throw DeletionLocalCleanupError.unsafeCaptureIdentifier }

        var urls: [URL] = []
        var referenceResolvable = true
        if let audioReference {
            if let referenced = staging.finalizedAudioURL(forReference: audioReference) {
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
    /// two names, since its file name already binds it to the deleted capture.
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

    private func boundInProgressNames(captureID: String) throws -> [String] {
        let continuationNames: Set<String> = [captureID + "-continued.wav", captureID + "-joined.wav"]
        return try fileSystem.entryNames(in: layout.directory(for: .ingressInProgressAudio))
            .filter { $0.hasPrefix(captureID + ".") || continuationNames.contains($0) }
            .sorted()
    }

    private func removeIfPresent(_ url: URL) throws {
        if fileSystem.fileExists(at: url) {
            try fileSystem.remove(at: url)
        }
        guard !fileSystem.fileExists(at: url) else { throw DeletionLocalCleanupError.stillPresent }
    }
}
