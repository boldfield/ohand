import Foundation

// The handoff from the native capture entry to the Tauri management shell. Only the capture identifier crosses, in
// exactly one URL shape: ohand-tauri://capture?captureId=<UPPERCASE-HYPHENATED-UUID>. The string is matched
// literally (no URLComponents, whose host/path split and normalisation differ from the Rust receiver in
// probes/tauri-handoff). Both sides are tested against probes/tauri-handoff/fixtures/handoff-urls.json.
// Foundation only: compiled into CaptureProbe, CaptureProbeControl and CaptureProbeTests.

enum ManagementHandoffError: String, Error, Equatable {
    case badScheme = "bad_scheme"
    case unknownRoute = "unknown_route"
    case missingCaptureId = "missing_capture_id"
    case unexpectedComponent = "unexpected_component"
    case invalidCaptureId = "invalid_capture_id"
}

enum ManagementHandoff {
    static let bundleIdentifier = "com.boldfield.ohand.tauri-probe"

    private static let urlPrefix = "ohand-tauri://"
    private static let route = "capture"
    private static let queryKeyPrefix = "captureId="
    private static let hyphenPositions: Set<Int> = [8, 13, 18, 23]
    private static let canonicalLength = 36

    /// True only for the form the entry mints: 36 characters, hyphens at 8/13/18/23, every other character 0-9 or A-F.
    static func isCanonicalCaptureId(_ candidate: String) -> Bool {
        let scalars = Array(candidate.unicodeScalars)
        guard scalars.count == canonicalLength else { return false }
        for (index, scalar) in scalars.enumerated() {
            if hyphenPositions.contains(index) {
                if scalar != "-" { return false }
            } else {
                let isDigit = scalar.value >= 0x30 && scalar.value <= 0x39
                let isUpperHex = scalar.value >= 0x41 && scalar.value <= 0x46
                if !isDigit && !isUpperHex { return false }
            }
        }
        return true
    }

    /// Returns the capture ID of a valid handoff URL, or the reason it was refused.
    static func captureId(fromHandoffURL text: String) -> Result<String, ManagementHandoffError> {
        guard text.hasPrefix(urlPrefix) else { return .failure(.badScheme) }
        let afterScheme = String(text.dropFirst(urlPrefix.count))
        let routeText: String
        let query: String?
        if let questionMark = afterScheme.firstIndex(of: "?") {
            routeText = String(afterScheme[..<questionMark])
            query = String(afterScheme[afterScheme.index(after: questionMark)...])
        } else {
            routeText = afterScheme
            query = nil
        }
        guard routeText == route else { return .failure(.unknownRoute) }
        guard let query = query, !query.isEmpty else { return .failure(.missingCaptureId) }
        guard query.hasPrefix(queryKeyPrefix) else { return .failure(.unexpectedComponent) }
        let candidate = String(query.dropFirst(queryKeyPrefix.count))
        return isCanonicalCaptureId(candidate) ? .success(candidate) : .failure(.invalidCaptureId)
    }

    /// The URL that asks the management shell to show a capture, or nil unless `captureId` is canonical.
    static func url(forCaptureId captureId: String) -> URL? {
        guard isCanonicalCaptureId(captureId) else { return nil }
        return URL(string: "\(urlPrefix)\(route)?\(queryKeyPrefix)\(captureId)")
    }
}

extension IngressOutcome {
    /// Offered only for an entry that is already committed. The save never waits on the management shell: the
    /// handoff is a separate, user-initiated step after the record exists.
    var managementHandoffURL: URL? {
        status.isFailure ? nil : ManagementHandoff.url(forCaptureId: captureId)
    }
}
