#if DEBUG
import UIKit

/// TEMPORARY (U10a diagnosis, reverted before review): with `-OhAndDiagAXFrames`, writes once a second the in-process
/// accessibility frames of the Home cards (what VoiceOver reads) next to their rendered frames, as the label of a
/// near-invisible element. With `-OhAndDiagPostLayout`, posts a layout-changed notification once a second.
@MainActor
final class HomeAXDiagnostic {
    static let shared = HomeAXDiagnostic()
    static let identifier = "diag.ax-frames"

    var rendered: [String: CGRect] = [:]
    private let label = UILabel(frame: CGRect(x: 0, y: 40, width: 4, height: 4))
    private var timer: Timer?

    private static var walkEnabled: Bool { ProcessInfo.processInfo.arguments.contains("-OhAndDiagAXFrames") }
    private static var layoutEnabled: Bool { ProcessInfo.processInfo.arguments.contains("-OhAndDiagPostLayout") }

    func start() {
        guard timer == nil, Self.walkEnabled || Self.layoutEnabled else { return }
        label.isAccessibilityElement = true
        label.accessibilityIdentifier = Self.identifier
        label.isUserInteractionEnabled = false
        label.alpha = 0.05
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            Task { @MainActor in HomeAXDiagnostic.shared.tick() }
        }
    }

    private func tick() {
        if Self.layoutEnabled { UIAccessibility.post(notification: .layoutChanged, argument: nil) }
        guard Self.walkEnabled else { return }
        guard let window = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).flatMap(\.windows).first else { return }
        if label.superview == nil { window.addSubview(label) }
        var found: [String] = []
        var visited = Set<ObjectIdentifier>()
        walk(window, depth: 0, visited: &visited, into: &found)
        let renderedText = rendered
            .filter { $0.key.hasPrefix("home.recents.card.") || $0.key == "home.recents.today" }
            .sorted { $0.key < $1.key }
            .map { "R \($0.key.suffix(12))|\(NSCoder.string(for: $0.value))" }
        label.accessibilityLabel = (found + renderedText).joined(separator: " ; ")
    }

    private func walk(_ object: NSObject, depth: Int, visited: inout Set<ObjectIdentifier>, into found: inout [String]) {
        guard depth < 60, visited.insert(ObjectIdentifier(object)).inserted, object !== label else { return }
        var identifier = ""
        if object.responds(to: NSSelectorFromString("accessibilityIdentifier")) {
            identifier = (object.value(forKey: "accessibilityIdentifier") as? String) ?? ""
        }
        let text = object.accessibilityLabel ?? ""
        if identifier.hasPrefix("home.recents.card.") || text == "Today" {
            found.append("AX \(identifier.suffix(12))|\(text.prefix(20))|\(NSCoder.string(for: object.accessibilityFrame))")
        }
        var children: [NSObject] = []
        if let elements = object.accessibilityElements as? [NSObject] {
            children = elements
        } else {
            let count = object.accessibilityElementCount()
            if count != NSNotFound, count > 0 {
                children = (0..<count).compactMap { object.accessibilityElement(at: $0) as? NSObject }
            }
        }
        if let view = object as? UIView { children += view.subviews }
        for child in children { walk(child, depth: depth + 1, visited: &visited, into: &found) }
    }
}
#endif
