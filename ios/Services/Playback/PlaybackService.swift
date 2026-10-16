import Foundation
import OhAndCoreBridge

/// Why retained audio could not be opened for playback. Nothing here carries a path.
public enum PlaybackError: Error, Equatable, Sendable {
    /// The read session is capture-only, or relocked before the open finished.
    case notAuthenticated
    case invalidRequest
    /// No readable item has that ID (the core does not say whether it is unknown, deleted or out of scope).
    case itemNotFound
    /// A deletion of the item was requested while the open was under way.
    case itemDeleted
    /// The item holds no audio, or its audio has been removed.
    case noAudio
    /// The audio reference does not name a file inside the finalized audio store.
    case audioReferenceUnresolvable
    /// The core says the item has audio, but the file is not there.
    case audioMissing
    /// The file is there but cannot be read or played, or whether it is there cannot be determined.
    case audioUnreadable
    case core(CoreFailure)
}

/// Opens an item's retained audio for playback without exposing protected storage.
///
/// * **Authenticated.** `open` needs an authenticated session, and the audio reference is read through the private read
///   gate, so a capture-only session never reaches the core and an answer that arrives after a relock is refused.
/// * **No path.** The result is a `PlaybackSession` that owns the player; callers get no URL or file name.
/// * **Fenced.** While a session is open the file is in the `PlaybackFence`: the retention sweep and deletion cleanup
///   leave it in place (retention reports `audioInPlayback` and retries later; cleanup fails and is retried).
/// * **Ended by the stronger decision.** A deletion request (`ItemDeletionService` tells the fence first), a relock
///   and shutdown end open sessions and stop their players; they do not wait for the listener.
public final class PlaybackService: @unchecked Sendable {
    private let core: RetentionControlCore
    private let session: ReadAuthenticationSession
    private let staging: IngressStagingStore
    private let fileSystem: AudioRetentionFileSystem
    let fence: PlaybackFence
    private let playerFactory: PlaybackPlayerFactory
    private let lock = NSLock()
    private var observerToken: SessionScopeObserverToken?

    init(
        core: RetentionControlCore,
        session: ReadAuthenticationSession,
        layout: ProtectedStorageLayout,
        fence: PlaybackFence,
        fileSystem: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem(),
        playerFactory: @escaping PlaybackPlayerFactory
    ) {
        self.core = core
        self.session = session
        self.fileSystem = fileSystem
        self.fence = fence
        self.playerFactory = playerFactory
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        let token = session.observeScope { [fence] transition in
            if transition.scope == .captureOnly { fence.endAll(reason: .relocked) }
        }
        observerToken = token
    }

    /// Opens the item's retained audio. The returned session must be closed when playback is done.
    public func open(itemID: String) async throws -> PlaybackSession {
        // The epoch this open was admitted in: a relock ends it even if the user authenticates again before it returns.
        guard let authorization = session.currentAuthorization() else { throw PlaybackError.notAuthenticated }
        guard !itemID.isEmpty else { throw PlaybackError.invalidRequest }
        let generation = fence.generation(of: itemID)
        let answer = try await readAudio(itemID: itemID)
        guard let reference = answer.audioReference else { throw PlaybackError.noAudio }
        guard let url = staging.finalizedAudioURL(forReference: reference) else {
            throw PlaybackError.audioReferenceUnresolvable
        }

        let box = SessionBox()
        let leaseID: UUID
        do {
            leaseID = try fence.admit(
                itemID: itemID, url: url, generation: generation,
                verify: {
                    // The read may have taken a while; a session that relocked since must not start playback.
                    guard session.isCurrent(authorization) else { throw PlaybackError.notAuthenticated }
                    let present: Bool
                    do { present = try fileSystem.isPresent(at: url) } catch { throw PlaybackError.audioUnreadable }
                    guard present else { throw PlaybackError.audioMissing }
                },
                end: { box.session?.end($0) })
        } catch PlaybackFenceError.deletionRequested {
            throw PlaybackError.itemDeleted
        }

        let player: PlaybackPlayer
        do {
            player = try playerFactory(url)
        } catch {
            fence.release(leaseID)
            throw PlaybackError.audioUnreadable
        }
        let opened = PlaybackSession(itemID: itemID, player: player, release: { [fence] in fence.release(leaseID) })
        box.session = opened
        // A relock or deletion between the lease and here found no session to end; end it now.
        if !session.isCurrent(authorization) {
            opened.end(.relocked)
            throw PlaybackError.notAuthenticated
        }
        if fence.generation(of: itemID) != generation {
            opened.end(.deletionRequested)
            throw PlaybackError.itemDeleted
        }
        return opened
    }

    /// Ends the item's open playback and refuses opens already under way. Called when a deletion is requested.
    func endPlayback(itemID: String) {
        fence.endPlayback(itemID: itemID)
    }

    /// Ends every open playback and stops watching the session. Called before the core closes.
    func shutDown() {
        lock.lock()
        let token = observerToken
        observerToken = nil
        lock.unlock()
        if let token { session.removeScopeObserver(token) }
        fence.endAll(reason: .relocked)
    }

    private final class SessionBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: PlaybackSession?

        var session: PlaybackSession? {
            get {
                lock.lock()
                defer { lock.unlock() }
                return stored
            }
            set {
                lock.lock()
                stored = newValue
                lock.unlock()
            }
        }
    }

    private func readAudio(itemID: String) async throws -> RetentionAudioAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<RetentionAudioAnswer, CoreFailure>, Never>) in
            core.readAudio(itemID: itemID) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw PlaybackError.itemNotFound
            case "invalid_request": throw PlaybackError.invalidRequest
            case "read_not_authenticated", "read_relocked": throw PlaybackError.notAuthenticated
            default: throw PlaybackError.core(failure)
            }
        }
    }
}
