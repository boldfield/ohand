import Foundation

/// An https origin reduced to the parts that decide whether two destinations are the same: lowercase host and
/// effective port. Scheme is always https, so an `http` URL can never equal an authorized origin.
struct ProviderOrigin: Equatable {
    let host: String
    let port: Int

    static let defaultPort = 443

    init(host: String, port: Int) {
        self.host = host
        self.port = port
    }

    /// Origin of a request or redirect URL. Nil for any URL that is not a plain https URL without userinfo and with
    /// a strictly well-formed host.
    init?(url: URL) {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
            components.scheme?.lowercased() == "https",
            components.user == nil,
            components.password == nil,
            let rawHost = components.host,
            let host = Self.normalizedHost(rawHost)
        else { return nil }
        let port = components.port ?? Self.defaultPort
        guard (1...65535).contains(port) else { return nil }
        self.init(host: host, port: port)
    }

    /// Parses an authorized destination as produced by core: exactly `https://host[:port]`, no path, query,
    /// fragment, userinfo or whitespace. Anything else cannot authorize a request.
    init?(bareOrigin: String) {
        let prefix = "https://"
        guard bareOrigin.lowercased().hasPrefix(prefix) else { return nil }
        let authority = String(bareOrigin.dropFirst(prefix.count))
        guard !authority.isEmpty,
            authority.unicodeScalars.allSatisfy({ !$0.properties.isWhitespace && $0.value >= 0x21 && $0.value < 0x7f }),
            !authority.contains(where: { "/?#@\\%".contains($0) })
        else { return nil }

        let hostText: String
        let portText: String?
        if authority.hasPrefix("[") {
            guard let close = authority.firstIndex(of: "]") else { return nil }
            hostText = String(authority[authority.startIndex...close])
            let rest = authority[authority.index(after: close)...]
            if rest.isEmpty {
                portText = nil
            } else if rest.hasPrefix(":") {
                portText = String(rest.dropFirst())
            } else {
                return nil
            }
        } else if let colon = authority.firstIndex(of: ":") {
            hostText = String(authority[..<colon])
            portText = String(authority[authority.index(after: colon)...])
        } else {
            hostText = authority
            portText = nil
        }

        guard let host = Self.normalizedHost(hostText) else { return nil }
        var port = Self.defaultPort
        if let portText {
            guard !portText.isEmpty, portText.allSatisfy({ $0.isASCII && $0.isNumber }),
                let parsed = Int(portText), (1...65535).contains(parsed)
            else { return nil }
            port = parsed
        }
        self.init(host: host, port: port)
    }

    /// Lowercases a host and accepts only DNS-style names, IPv4 literals and IPv6 literals. Rejecting everything
    /// else (percent escapes, backslashes, whitespace) keeps this check and URLSession's parse in agreement.
    private static func normalizedHost(_ rawHost: String) -> String? {
        var host = rawHost.lowercased()
        if host.hasPrefix("[") && host.hasSuffix("]") {
            host = String(host.dropFirst().dropLast())
        }
        guard !host.isEmpty, host.count <= 253 else { return nil }
        if host.contains(":") {
            let isIPv6 = host.allSatisfy { $0.isHexDigit || $0 == ":" || $0 == "." }
            return isIPv6 ? host : nil
        }
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        let valid = labels.allSatisfy { label in
            !label.isEmpty && label.count <= 63 && !label.hasPrefix("-") && !label.hasSuffix("-")
                && label.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }
        }
        return valid ? host : nil
    }
}
