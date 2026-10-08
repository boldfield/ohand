import Foundation

enum HandoffRoute: String, Equatable {
    case capture
}

struct HandoffRequest: Equatable {
    let captureId: UUID
    let route: HandoffRoute
    let timestamp: String
}

enum HandoffError: Equatable, CustomStringConvertible {
    case invalidURL
    case missingCaptureId
    case invalidCaptureId
    case duplicateCaptureId
    case invalidRoute
    case missingRoute

    var description: String {
        switch self {
        case .invalidURL: return "Invalid handoff URL"
        case .missingCaptureId: return "Missing captureId parameter"
        case .invalidCaptureId: return "Invalid captureId: must be a valid UUID"
        case .duplicateCaptureId: return "Duplicate captureId parameters"
        case .invalidRoute: return "Invalid route parameter"
        case .missingRoute: return "Missing route in URL host"
        }
    }
}

struct HandoffValidator {
    static let scheme = "ohand-tauri"

    static func validate(url: URL) -> Result<HandoffRequest, HandoffError> {
        guard url.scheme?.lowercased() == scheme else {
            return .failure(.invalidURL)
        }

        guard let hostComponent = url.host, !hostComponent.isEmpty else {
            return .failure(.missingRoute)
        }

        guard let route = HandoffRoute(rawValue: hostComponent) else {
            return .failure(.invalidRoute)
        }

        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: true),
              let queryItems = components.queryItems else {
            return .failure(.missingCaptureId)
        }

        let captureIdItems = queryItems.filter { $0.name == "captureId" }

        guard !captureIdItems.isEmpty else {
            return .failure(.missingCaptureId)
        }

        guard captureIdItems.count == 1 else {
            return .failure(.duplicateCaptureId)
        }

        guard let captureIdValue = captureIdItems[0].value, !captureIdValue.isEmpty else {
            return .failure(.missingCaptureId)
        }

        guard let captureId = UUID(uuidString: captureIdValue) else {
            return .failure(.invalidCaptureId)
        }

        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let timestamp = formatter.string(from: Date())

        return .success(HandoffRequest(
            captureId: captureId,
            route: route,
            timestamp: timestamp
        ))
    }
}
