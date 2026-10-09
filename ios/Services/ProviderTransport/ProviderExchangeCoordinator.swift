import Foundation
import OhAndCoreBridge

/// Sends one prepared provider request. `ProviderHTTPTransport` is the production implementation; tests wrap it.
public protocol ProviderRequestSender: AnyObject {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse
}

extension ProviderHTTPTransport: ProviderRequestSender {}

/// Connects the core's provider exchange to the secure native transport.
///
/// The core decides everything about a request at dispatch: destination, headers, credential reference and the
/// authorized origins all come from its stored policy and are carried in the send command. This type adds nothing
/// to them and never sees a secret: the transport resolves the credential reference from the Keychain when it
/// sends. A send runs in its own task so the core's exchange thread returns immediately; a cancel command cancels
/// that task, which stops the HTTP exchange. Each send is answered exactly once, with the response or with the
/// transport error's core name.
public final class ProviderExchangeCoordinator: @unchecked Sendable {
    private let core: CoreHandle
    private let sender: ProviderRequestSender
    private let stateLock = NSLock()
    private var runningSends: [UInt64: Task<Void, Never>] = [:]
    private var registration: ProviderTransportRegistration?
    private var isShutDown = false

    private init(core: CoreHandle, sender: ProviderRequestSender) {
        self.core = core
        self.sender = sender
    }

    /// Registers `sender` as the core's provider transport. Call `shutDown()` before closing the core.
    public static func attach(to core: CoreHandle, sender: ProviderRequestSender) throws -> ProviderExchangeCoordinator {
        let coordinator = ProviderExchangeCoordinator(core: core, sender: sender)
        coordinator.registration = try core.registerProviderTransport { [weak coordinator] operationID, command in
            coordinator?.handle(operationID: operationID, command: command)
        }
        return coordinator
    }

    /// Cancels running sends and unregisters from the core. Idempotent. Must not be called from a core callback.
    public func shutDown() {
        stateLock.lock()
        guard !isShutDown else {
            stateLock.unlock()
            return
        }
        isShutDown = true
        let cancelledSends = Array(runningSends.values)
        let activeRegistration = registration
        registration = nil
        stateLock.unlock()

        cancelledSends.forEach { $0.cancel() }
        activeRegistration?.invalidate()
    }

    private func handle(operationID: UInt64, command: ProviderTransportCommand) {
        switch command {
        case .send(let description):
            startSend(operationID: operationID, description: description)
        case .cancel:
            stateLock.lock()
            let send = runningSends[operationID]
            stateLock.unlock()
            send?.cancel()
        }
    }

    private func startSend(operationID: UInt64, description: ProviderSendCommand) {
        guard let request = Self.makeRequest(description) else {
            fail(operationID: operationID, error: "rejected")
            return
        }
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !isShutDown else {
            fail(operationID: operationID, error: "cancelled")
            return
        }
        // The task's cleanup takes the lock, so it cannot run before the task is recorded.
        runningSends[operationID] = Task { [self] in
            await perform(request, operationID: operationID)
            stateLock.lock()
            runningSends[operationID] = nil
            stateLock.unlock()
        }
    }

    private func perform(_ request: ProviderHTTPRequest, operationID: UInt64) async {
        do {
            let response = try await sender.send(request)
            try? core.completeProviderExchange(
                operationID: operationID, status: response.status, headers: response.headers, body: response.body)
        } catch let error as ProviderTransportError {
            fail(operationID: operationID, error: error.coreTransportError)
        } catch {
            fail(operationID: operationID, error: "rejected")
        }
    }

    private func fail(operationID: UInt64, error: String) {
        try? core.failProviderExchange(operationID: operationID, error: error)
    }

    private static func makeRequest(_ description: ProviderSendCommand) -> ProviderHTTPRequest? {
        guard let url = URL(string: description.url),
            let method = ProviderHTTPMethod(rawValue: description.method),
            description.timeoutMilliseconds > 0
        else { return nil }
        var headers: [String: String] = [:]
        for pair in description.headers {
            guard pair.count == 2 else { return nil }
            headers[pair[0]] = pair[1]
        }
        let credential = description.credential.map {
            ProviderCredentialAttachment(reference: $0.reference, headerName: $0.header, scheme: $0.scheme)
        }
        return ProviderHTTPRequest(
            url: url,
            method: method,
            headers: headers,
            body: Data(description.body.utf8),
            timeout: TimeInterval(description.timeoutMilliseconds) / 1000,
            maxResponseBytes: description.maxResponseBytes,
            credential: credential,
            authorization: ProviderTransportAuthorization(
                jobID: String(description.operationID),
                capability: .textInterpretation,
                authorizedOrigins: description.authorizedOrigins))
    }
}
