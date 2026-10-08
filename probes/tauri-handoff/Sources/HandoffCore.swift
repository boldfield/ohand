import Foundation

enum HandoffRoute: String, Equatable {
    case capture
}

struct HandoffRequest: Equatable {
    let captureId: String
    let route: HandoffRoute
    let timestamp: String
}

enum HandoffError: Equatable, CustomStringConvertible {
    case invalidURL
    case missingCaptureId
    case invalidRoute
    case missingRoute

    var description: String {
        switch self {
        case .invalidURL: return "Invalid handoff URL"
        case .missingCaptureId: return "Missing captureId parameter"
        case .invalidRoute: return "Invalid route parameter"
        case .missingRoute: return "Missing route in URL path"
        }
    }
}

struct HandoffValidator {
    static let scheme = "ohand-tauri"

    static func validate(url: URL) -> Result<HandoffRequest, HandoffError> {
        guard url.scheme?.lowercased() == scheme else {
            return .failure(.invalidURL)
        }

        let path = url.path
        guard !path.isEmpty else {
            return .failure(.missingRoute)
        }

        let routeComponent = String(path.dropFirst())
        guard let route = HandoffRoute(rawValue: routeComponent) else {
            return .failure(.invalidRoute)
        }

        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: true),
              let queryItems = components.queryItems else {
            return .failure(.missingCaptureId)
        }

        guard let captureIdItem = queryItems.first(where: { $0.name == "captureId" }),
              let captureId = captureIdItem.value,
              !captureId.isEmpty else {
            return .failure(.missingCaptureId)
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
