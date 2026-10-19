import Foundation
import OhAndCoreBridge

/// Why retained audio could not be opened for playback. Nothing here carries a path.
public enum PlaybackError: Error, Equatable, Sendable {
    /// The read session is capture-only, or relocked before the open finished.
    case notAuthenticated
    case invalidRequest
    /// The service shut down before the open finished.
    case shutDown
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

/// Whether an item's audio can be played now, as `PlaybackService.availability(itemID:)` found it.
public enum PlaybackAvailability: Equatable, Sendable {
    case available
    /// The item holds no audio, or its audio has been removed.
    case noAudio
    /// The core says the item has audio, but the file is not there.
    case missing
    /// The file cannot be checked, or the reference does not name a file in the finalized audio store.
    case unreadable
    case notAuthenticated
}

/// Opens an item's retained audio for playback without exposing protected storage.
///
/// * **Authenticated.** `open` needs an authenticated session, and the audio reference is read through the private read
///   gate, so a capture-only session never reaches the core and an answer that arrives after a relock is refused.
/// * **No path.** The result is a `PlaybackSession` that owns the player; callers get no URL or file name.
/// * **Fenced.** While a session is open the file is in the `PlaybackFence`: the retention sweep and deletion cleanup
///   leave it in place (retention reports `audioInPlayback` and retries later; cleanup fails and is retried).
/// * **Ended by the stronger decision.** A deletion request (`ItemDeletionService` tells the fence first), a relock
///   and shutdown end open sessions and stop their players; they do not wait for the listener. An open still under way
///   when one of them happens is refused, and the player it built is stopped.
public final class PlaybackService: @unchecked Sendable {
    private let core: RetentionControlCore
    private let session: ReadAuthenticationSession
    private let staging: IngressStagingStore
    private let fileSystem: AudioRetentionFileSystem
    let fence: PlaybackFence
    private let playerFactory: PlaybackPlayerFactory
    private let lock = NSLock()
    private var observerToken: SessionScopeObserverToken?
    private var isShutDown = false

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
        guard !hasShutDown else { throw PlaybackError.shutDown }
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
                    // Shutdown marks the service before ending sessions, so an open admitted after that is refused here.
                    guard !hasShutDown else { throw PlaybackError.shutDown }
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
        // A relock, deletion or shutdown between the lease and here found no session to end; end it now.
        if hasShutDown {
            opened.end(.relocked)
            throw PlaybackError.shutDown
        }
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

    /// Whether the item's audio can be played now, by the checks `open` makes before the fence. Read-only: it takes no
    /// lease, builds no player and reads no audio bytes. The core's reference can outlive a file that retention removed,
    /// so the file is checked here. A removal after this answer is still caught by `open`.
    public func availability(itemID: String) async -> PlaybackAvailability {
        guard let authorization = session.currentAuthorization() else { return .notAuthenticated }
        guard !itemID.isEmpty, !hasShutDown else { return .unreadable }
        let answer: RetentionAudioAnswer
        do {
            answer = try await readAudio(itemID: itemID)
        } catch PlaybackError.notAuthenticated {
            return .notAuthenticated
        } catch PlaybackError.itemNotFound {
            return .noAudio
        } catch {
            return .unreadable
        }
        // An answer that arrives after a relock is not shown.
        guard session.isCurrent(authorization) else { return .notAuthenticated }
        guard let reference = answer.audioReference else { return .noAudio }
        guard let url = staging.finalizedAudioURL(forReference: reference) else { return .unreadable }
        do {
            return try fileSystem.isPresent(at: url) ? .available : .missing
        } catch {
            return .unreadable
        }
    }

    /// Ends the item's open playback and refuses opens already under way. Called when a deletion is requested.
    func endPlayback(itemID: String) {
        fence.endPlayback(itemID: itemID)
    }

    /// Ends every open playback, refuses opens from now on (including those under way) and stops watching the session.
    /// Called before the core closes.
    func shutDown() {
        lock.lock()
        isShutDown = true
        let token = observerToken
        observerToken = nil
        lock.unlock()
        if let token { session.removeScopeObserver(token) }
        fence.endAll(reason: .relocked)
    }

    private var hasShutDown: Bool {
        lock.lock()
        defer { lock.unlock() }
        return isShutDown
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
