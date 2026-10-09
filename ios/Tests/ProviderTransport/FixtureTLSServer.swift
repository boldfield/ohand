import Foundation
import Network
import Security

/// A loopback HTTPS/1.1 server for transport tests. It records every request it parses and answers each with the
/// scripted `Response`, so tests can assert both what the client did and what the server never saw.
final class FixtureTLSServer {
    struct RecordedRequest {
        let method: String
        let target: String
        let headers: [String: String]
        let body: Data
    }

    enum Response {
        case complete(status: Int, headers: [String: String] = [:], body: Data = Data())
        /// Reads the request and never answers.
        case hang
        /// Sends headers without a content length, then body chunks until the client goes away.
        case endlessBody(chunk: Data)
    }

    enum StartFailure: Error {
        case listener
        case timedOut
    }

    private static let maximumEndlessChunks = 4096

    private let queue = DispatchQueue(label: "com.boldfield.ohand.tests.fixture-tls-server")
    private let listener: NWListener
    private let handler: (RecordedRequest) -> Response
    private let lock = NSLock()
    private var recorded: [RecordedRequest] = []
    private var accepted = 0
    private var connections: [ObjectIdentifier: NWConnection] = [:]
    private let stopped = DispatchSemaphore(value: 0)
    private var isStopped = false
    private(set) var port: UInt16 = 0

    init(identity: SecIdentity, handler: @escaping (RecordedRequest) -> Response) throws {
        let tlsOptions = NWProtocolTLS.Options()
        guard let secIdentity = sec_identity_create(identity) else { throw StartFailure.listener }
        sec_protocol_options_set_local_identity(tlsOptions.securityProtocolOptions, secIdentity)
        sec_protocol_options_add_tls_application_protocol(tlsOptions.securityProtocolOptions, "http/1.1")
        let parameters = NWParameters(tls: tlsOptions)
        parameters.requiredInterfaceType = .loopback
        self.listener = try NWListener(using: parameters)
        self.handler = handler
    }

    var origin: String { "https://localhost:\(port)" }
    var baseURL: URL { URL(string: origin)! }

    var requests: [RecordedRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    /// Connections the listener accepted, including ones that never completed a request.
    var acceptedConnectionCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return accepted
    }

    func start() throws {
        let ready = DispatchSemaphore(value: 0)
        var failed = false
        listener.stateUpdateHandler = { [self] state in
            switch state {
            case .ready:
                ready.signal()
            case .failed:
                failed = true
                ready.signal()
            case .cancelled:
                self.stopped.signal()
            default:
                break
            }
        }
        listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
        listener.start(queue: queue)
        guard ready.wait(timeout: .now() + 10) == .success else { throw StartFailure.timedOut }
        guard !failed, let listenerPort = listener.port else { throw StartFailure.listener }
        port = listenerPort.rawValue
    }

    /// Returns once the listener has stopped accepting, so a later connect to `port` is refused.
    func stop() {
        lock.lock()
        let alreadyStopped = isStopped
        isStopped = true
        lock.unlock()
        if !alreadyStopped {
            listener.cancel()
            _ = stopped.wait(timeout: .now() + 5)
        }
        lock.lock()
        let open = Array(connections.values)
        connections.removeAll()
        lock.unlock()
        open.forEach { $0.cancel() }
    }

    private func accept(_ connection: NWConnection) {
        lock.lock()
        accepted += 1
        connections[ObjectIdentifier(connection)] = connection
        lock.unlock()
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let connection else { return }
            switch state {
            case .failed, .cancelled:
                self?.forget(connection)
            default:
                break
            }
        }
        connection.start(queue: queue)
        receive(on: connection, buffer: Data())
    }

    private func forget(_ connection: NWConnection) {
        lock.lock()
        connections.removeValue(forKey: ObjectIdentifier(connection))
        lock.unlock()
    }

    private func receive(on connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            var buffer = buffer
            if let data { buffer.append(data) }
            if let request = Self.parse(buffer) {
                self.lock.lock()
                self.recorded.append(request)
                self.lock.unlock()
                self.respond(to: connection, with: self.handler(request))
            } else if error != nil || isComplete {
                connection.cancel()
            } else {
                self.receive(on: connection, buffer: buffer)
            }
        }
    }

    /// Returns a request once the header block and the declared body are fully buffered.
    private static func parse(_ buffer: Data) -> RecordedRequest? {
        guard let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)) else { return nil }
        let headerText = String(decoding: buffer[..<headerEnd.lowerBound], as: UTF8.self)
        var lines = headerText.components(separatedBy: "\r\n")
        let requestLine = lines.removeFirst().split(separator: " ")
        guard requestLine.count >= 2 else { return nil }
        var headers: [String: String] = [:]
        for line in lines {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = line[..<colon].lowercased()
            headers[name] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        let body = buffer[headerEnd.upperBound...]
        let declaredLength = Int(headers["content-length"] ?? "0") ?? 0
        guard body.count >= declaredLength else { return nil }
        return RecordedRequest(
            method: String(requestLine[0]), target: String(requestLine[1]), headers: headers,
            body: Data(body.prefix(declaredLength)))
    }

    private func respond(to connection: NWConnection, with response: Response) {
        switch response {
        case .hang:
            return
        case .complete(let status, let headers, let body):
            var head = "HTTP/1.1 \(status) Fixture\r\nContent-Length: \(body.count)\r\nConnection: close\r\n"
            for (name, value) in headers { head += "\(name): \(value)\r\n" }
            head += "\r\n"
            connection.send(
                content: Data(head.utf8) + body, contentContext: .finalMessage, isComplete: true,
                completion: .contentProcessed { _ in connection.cancel() })
        case .endlessBody(let chunk):
            let head = Data("HTTP/1.1 200 Fixture\r\nConnection: close\r\n\r\n".utf8)
            connection.send(
                content: head, contentContext: .defaultMessage, isComplete: false,
                completion: .contentProcessed { [weak self] error in
                    guard error == nil else { return }
                    self?.sendChunks(chunk, remaining: Self.maximumEndlessChunks, on: connection)
                })
        }
    }

    private func sendChunks(_ chunk: Data, remaining: Int, on connection: NWConnection) {
        guard remaining > 0 else {
            connection.cancel()
            return
        }
        connection.send(
            content: chunk, contentContext: .defaultMessage, isComplete: false,
            completion: .contentProcessed { [weak self] error in
                guard error == nil else { return }
                self?.sendChunks(chunk, remaining: remaining - 1, on: connection)
            })
    }
}
