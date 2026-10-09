import Foundation
import Security

/// One request/response exchange on its own ephemeral `URLSession`.
///
/// The first outcome wins: `finish` is idempotent, so a timeout, cancellation, size violation or delegate failure
/// ends the exchange exactly once and every later callback is ignored. Delegate callbacks run on a private serial
/// queue; `lock` guards the state that `run`, `cancel` and the timeout timer share with it.
final class ProviderTransportOperation: NSObject, URLSessionDataDelegate {
    static let maximumRedirects = 5

    private let urlRequest: URLRequest
    private let origin: ProviderOrigin
    private let timeout: TimeInterval
    private let maxResponseBytes: Int
    private let trustAnchors: [SecCertificate]

    private let lock = NSLock()
    private var continuation: CheckedContinuation<ProviderHTTPResponse, Error>?
    private var session: URLSession?
    private var timeoutTimer: DispatchWorkItem?
    private var finished = false
    private var cancelRequested = false

    // Touched only on the delegate queue.
    private var receivedResponse: HTTPURLResponse?
    private var receivedBody = Data()
    private var redirectCount = 0

    init(
        urlRequest: URLRequest, origin: ProviderOrigin, timeout: TimeInterval, maxResponseBytes: Int,
        trustAnchors: [SecCertificate]
    ) {
        self.urlRequest = urlRequest
        self.origin = origin
        self.timeout = timeout
        self.maxResponseBytes = maxResponseBytes
        self.trustAnchors = trustAnchors
    }

    func run() async throws -> ProviderHTTPResponse {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<ProviderHTTPResponse, Error>) in
                self.begin(continuation)
            }
        } onCancel: {
            self.cancel()
        }
    }

    private func begin(_ continuation: CheckedContinuation<ProviderHTTPResponse, Error>) {
        lock.lock()
        if cancelRequested {
            lock.unlock()
            continuation.resume(throwing: ProviderTransportError.cancelled)
            return
        }

        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = timeout
        configuration.timeoutIntervalForResource = timeout
        configuration.tlsMinimumSupportedProtocolVersion = .TLSv12

        let delegateQueue = OperationQueue()
        delegateQueue.maxConcurrentOperationCount = 1
        delegateQueue.name = "com.boldfield.ohand.provider-transport"
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: delegateQueue)

        self.continuation = continuation
        self.session = session
        let timer = DispatchWorkItem { [weak self] in self?.finish(.failure(.timeout)) }
        timeoutTimer = timer
        // Created under the lock: `finish` invalidates the session, and creating a task on an invalidated session
        // is a programmer error. Resuming a task whose session was invalidated afterwards is harmless.
        let task = session.dataTask(with: urlRequest)
        lock.unlock()

        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timer)
        task.resume()
    }

    private func cancel() {
        lock.lock()
        cancelRequested = true
        let started = continuation != nil
        lock.unlock()
        if started {
            finish(.failure(.cancelled))
        }
    }

    private func finish(_ result: Result<ProviderHTTPResponse, ProviderTransportError>) {
        lock.lock()
        if finished {
            lock.unlock()
            return
        }
        finished = true
        let continuation = self.continuation
        let session = self.session
        let timer = timeoutTimer
        self.continuation = nil
        self.session = nil
        timeoutTimer = nil
        lock.unlock()

        timer?.cancel()
        session?.invalidateAndCancel()
        switch result {
        case .success(let response):
            continuation?.resume(returning: response)
        case .failure(let error):
            continuation?.resume(throwing: error)
        }
    }

    // MARK: Trust

    func urlSession(
        _ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, completionHandler)
    }

    func urlSession(
        _ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, completionHandler)
    }

    /// Server trust is evaluated by the system, hostname included. Test anchors replace the system roots but are
    /// evaluated by the same policy. No other challenge is answered with a credential, so a Basic or client
    /// certificate request falls through and the server's own 401 reaches the caller as a response.
    private func answer(
        _ challenge: URLAuthenticationChallenge,
        _ completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        guard !trustAnchors.isEmpty else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        guard let trust = challenge.protectionSpace.serverTrust else {
            finish(.failure(.tlsValidationFailed))
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        SecTrustSetAnchorCertificates(trust, trustAnchors as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var evaluationError: CFError?
        if SecTrustEvaluateWithError(trust, &evaluationError) {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            finish(.failure(.tlsValidationFailed))
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }

    // MARK: Redirects

    func urlSession(
        _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
    ) {
        redirectCount += 1
        guard redirectCount <= Self.maximumRedirects, let target = request.url,
            ProviderOrigin(url: target) == origin
        else {
            finish(.failure(.redirectRefused))
            completionHandler(nil)
            return
        }
        // URLSession strips Authorization on redirects; the target is the same origin, so restore what we sent.
        var followed = request
        for (name, value) in urlRequest.allHTTPHeaderFields ?? [:] where followed.value(forHTTPHeaderField: name) == nil {
            followed.setValue(value, forHTTPHeaderField: name)
        }
        completionHandler(followed)
    }

    // MARK: Response

    func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        guard let http = response as? HTTPURLResponse else {
            finish(.failure(.invalidResponse))
            completionHandler(.cancel)
            return
        }
        if http.expectedContentLength > Int64(maxResponseBytes) {
            finish(.failure(.responseTooLarge))
            completionHandler(.cancel)
            return
        }
        receivedResponse = http
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        receivedBody.append(data)
        if receivedBody.count > maxResponseBytes {
            receivedBody = Data()
            finish(.failure(.responseTooLarge))
            dataTask.cancel()
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error {
            finish(.failure(Self.normalized(error)))
            return
        }
        guard let http = receivedResponse else {
            finish(.failure(.invalidResponse))
            return
        }
        finish(.success(ProviderHTTPResponse(status: http.statusCode, headers: Self.headers(of: http), body: receivedBody)))
    }

    private static func headers(of response: HTTPURLResponse) -> [String: String] {
        var headers: [String: String] = [:]
        for (key, value) in response.allHeaderFields {
            guard let name = (key as? String)?.lowercased(), name != "set-cookie" else { continue }
            headers[name] = String(describing: value)
        }
        return headers
    }

    static func normalized(_ error: Error) -> ProviderTransportError {
        guard let urlError = error as? URLError else { return .connectionFailed }
        switch urlError.code {
        case .timedOut:
            return .timeout
        case .cancelled:
            return .cancelled
        case .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateHasUnknownRoot,
            .serverCertificateNotYetValid, .secureConnectionFailed, .clientCertificateRejected,
            .clientCertificateRequired, .appTransportSecurityRequiresSecureConnection:
            return .tlsValidationFailed
        case .httpTooManyRedirects, .redirectToNonExistentLocation:
            return .redirectRefused
        case .dataLengthExceedsMaximum:
            return .responseTooLarge
        case .badURL, .unsupportedURL:
            return .invalidRequest(.malformedURL)
        default:
            return .connectionFailed
        }
    }
}
