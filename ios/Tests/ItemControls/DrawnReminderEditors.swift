import UIKit
@testable import OhAnd

/// Counts the reminder editors drawn in a hosted window, so a test can tell one editor from two.
///
/// Each `ReminderEditSection` draws one date picker and one Save button. Both are looked for: date pickers in the view
/// tree, and Save buttons by their accessibility identifier in the view and accessibility trees. The larger count is the
/// number of editors drawn.
enum DrawnReminderEditors {
    static func count(in window: UIWindow?) -> Int {
        guard let window else { return 0 }
        var datePickers = 0
        var saveButtons = 0
        var visited = Set<ObjectIdentifier>()
        func visit(_ element: NSObject, depth: Int) {
            guard depth < 200, visited.insert(ObjectIdentifier(element)).inserted else { return }
            if element is UIDatePicker { datePickers += 1 }
            if (element as? UIAccessibilityIdentification)?.accessibilityIdentifier == ReminderEditSection.saveIdentifier {
                saveButtons += 1
            }
            if let view = element as? UIView { view.subviews.forEach { visit($0, depth: depth + 1) } }
            if let elements = element.accessibilityElements as? [NSObject] {
                elements.forEach { visit($0, depth: depth + 1) }
            } else {
                let elementCount = element.accessibilityElementCount()
                if elementCount != NSNotFound, elementCount > 0 {
                    for index in 0..<elementCount {
                        if let child = element.accessibilityElement(at: index) as? NSObject { visit(child, depth: depth + 1) }
                    }
                }
            }
        }
        visit(window, depth: 0)
        return max(datePickers, saveButtons)
    }

    /// The picker-like views in the window, named in a failure message when the count is not what a test expected.
    static func describe(in window: UIWindow?) -> String {
        guard let window else { return "no window" }
        var names: [String] = []
        func visit(_ view: UIView) {
            let name = String(describing: type(of: view))
            if name.contains("Picker") || name.contains("Date") { names.append(name) }
            view.subviews.forEach(visit)
        }
        visit(window)
        return names.isEmpty ? "no picker views" : names.joined(separator: ", ")
    }
}
