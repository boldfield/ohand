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

    /// Removes the staging record, an interrupted write of it, and the audio files the record hands off. Audio goes
    /// first and the record last: a record that survives an interruption still names the audio left to remove.
    func clearIngress(captureID: String) throws {
        guard IngressRecord.isSafeCaptureID(captureID) else { throw DeletionLocalCleanupError.unsafeCaptureIdentifier }

        if let record = try readableRecord(captureID: captureID), let audio = record.audio {
            try removeIfPresent(staging.inProgressAudioURL(fileName: audio.inProgressFileName))
            try removeIfPresent(staging.finalizedAudioURL(fileName: audio.finalizedFileName))
        }
        let recordURL = staging.recordURL(captureID: captureID)
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

    /// The staging record, or nil when it is absent or cannot be parsed. A record that exists but cannot be read now
    /// (for example while the device is locked) throws, because treating it as absent would leave its audio behind.
    /// A corrupt record is deleted regardless: its file name binds it to the deleted capture. Its hand-off names are
    /// unknown, so the in-progress audio it claimed is not removed here; ingress recovery reports that file as unclaimed
    /// in-progress audio (`IngressRecoveryReport`) and leaves it to the recorder's own recovery.
    private func readableRecord(captureID: String) throws -> IngressRecord? {
        do {
            return try staging.load(captureID: captureID)
        } catch IngressStagingFailure.corruptRecord {
            return nil
        } catch IngressStagingFailure.unsupportedRecordVersion {
            return nil
        } catch IngressStagingFailure.invalidRecord {
            return nil
        }
    }

    private func removeIfPresent(_ url: URL) throws {
        if fileSystem.fileExists(at: url) {
            try fileSystem.remove(at: url)
        }
        guard !fileSystem.fileExists(at: url) else { throw DeletionLocalCleanupError.stillPresent }
    }
}
