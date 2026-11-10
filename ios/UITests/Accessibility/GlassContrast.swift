import CoreGraphics
import Foundation

/// One element on screen at an audit stop, as the pre-audit snapshot shows it.
struct ScreenElement: Equatable {
    /// The node's position in the snapshot walk, and the positions of the nodes that contain it in the hierarchy.
    var nodeID = 0
    var ancestorIDs: Set<Int> = []
    /// Identifies the same element across scroll stops: the enclosing Home card (when there is one), the identifier and
    /// the label, with a card caption's age dropped ("Idea · 13 sec. ago" is "Idea" in its card).
    var key: String
    let identifier: String
    let label: String
    let frame: CGRect

    /// "Idea · 13 sec. ago" → "Idea": the age in a card caption changes from one stop to the next.
    static func stableLabel(_ label: String) -> String {
        guard let separator = label.range(of: " \u{00B7} ") else { return label }
        return String(label[..<separator.lowerBound])
    }

    static func key(card: String, identifier: String, label: String) -> String {
        "\(card)|\(identifier)|\(stableLabel(label))"
    }

    /// A Home card named by what it says, without the age in its caption ("8 sec. ago"), so the same card has the same
    /// name in every launch and at both text sizes (its identifier is a new UUID each launch).
    static func cardName(_ cardLabel: String) -> String {
        cardLabel.replacingOccurrences(of: "[0-9]+ [A-Za-z]+\\.? ago", with: "", options: .regularExpression)
    }
}

/// How an element in the glass list relates to the glass.
enum GlassPlacement: Equatable {
    /// Content that scrolls under a glass overlay (the header buttons, the back button, the composer). It is unobstructed
    /// when its frame lies wholly inside the clear band between the overlays and on screen.
    case scrollsUnderGlass
    /// Text drawn on a glass surface itself (the composer placeholder, the header mark). It is unobstructed when no other
    /// element's frame overlaps it, that is when nothing is behind the glass there and nothing else is drawn on it.
    case drawnOnGlass
}

/// Decides the contrast issues the audit reports for the elements in the glass list, and keeps the proof each test owes.
///
/// The audit measures text under or on a translucent glass overlay against the blurred overlay, which it cannot resolve.
/// An element in the list is excused at a stop only where it is obstructed, and only until the same test audits it
/// unobstructed: an element scrolled under glass must be audited wholly clear of it at some stop, with no contrast issue;
/// text drawn on glass must be audited, or measured from the screenshot, at a stop where nothing overlaps it. A contrast
/// issue on an unobstructed element always fails, and so does an excuse the test never settles (`unsettled`).
struct GlassContrastLedger {
    enum Decision: Equatable {
        /// A real finding: the element is unobstructed and the audit (or the measurement) says it fails.
        case issue(String)
        /// Excused at this stop; the test still owes an unobstructed audit of the element.
        case owed
        /// Excused by a measurement from the screenshot at an unobstructed stop (text drawn on glass only).
        case measured(Double)
    }

    /// The WCAG AA ratio for body text, applied to every element in the list, large or not.
    static let minimumContrastRatio = 4.5

    /// Keys excused at some stop, with the first stop that excused them.
    private(set) var owed: [String: String] = [:]
    /// Keys audited unobstructed at some stop with no contrast issue, or measured there at the minimum or above.
    private(set) var settled: [String: String] = [:]

    static func isUnobstructed(
        _ element: ScreenElement, placement: GlassPlacement, clearBand: ClosedRange<CGFloat>, screen: CGRect,
        others: [ScreenElement]
    ) -> Bool {
        switch placement {
        case .scrollsUnderGlass:
            let frame = element.frame
            return !frame.isEmpty && screen.contains(frame)
                && frame.minY >= clearBand.lowerBound && frame.maxY <= clearBand.upperBound
        case .drawnOnGlass:
            return overlapping(element, in: others, screen: screen).isEmpty && screen.contains(element.frame)
        }
    }

    /// The elements whose frames overlap `element`, other than its ancestors and descendants in the hierarchy (the pill
    /// around the mark, the field around its placeholder) and an element with the same label (the same control). A card
    /// behind the composer overlaps the placeholder even when its frame contains it, because it is not its ancestor.
    /// A node whose frame contains the whole `screen` is a container (iOS 27 reports a labelled `Toolbar` of the screen's
    /// size) and obstructs nothing, whatever its label or type; every smaller node, a container's labelled descendants
    /// included, still does.
    static func overlapping(_ element: ScreenElement, in others: [ScreenElement], screen: CGRect) -> [ScreenElement] {
        others.filter { other in
            guard other.key != element.key, other.label != element.label, !other.frame.isEmpty, !other.frame.contains(screen),
                  !element.ancestorIDs.contains(other.nodeID), !other.ancestorIDs.contains(element.nodeID) else { return false }
            let shared = other.frame.intersection(element.frame)
            return !shared.isNull && shared.width > 1 && shared.height > 1
        }
    }

    /// A contrast issue on `element` (nil when the snapshot before the audit did not show it on screen). `measure` is
    /// asked for the screenshot contrast only for text drawn on glass at an unobstructed stop.
    mutating func decide(
        contrastIssueOn element: ScreenElement?, placement: GlassPlacement, screenName: String,
        clearBand: ClosedRange<CGFloat>, screen: CGRect, others: [ScreenElement], measure: () -> Double?
    ) -> Decision {
        guard let element else { return .issue("the element is not on screen in the snapshot taken before the audit") }
        guard Self.isUnobstructed(element, placement: placement, clearBand: clearBand, screen: screen, others: others) else {
            if owed[element.key] == nil { owed[element.key] = screenName }
            return .owed
        }
        switch placement {
        case .scrollsUnderGlass:
            return .issue("the element lies wholly clear of the glass, so its contrast issue is real")
        case .drawnOnGlass:
            guard let ratio = measure() else { return .issue("the contrast could not be measured from the screenshot") }
            guard ratio >= Self.minimumContrastRatio else {
                return .issue(String(format: "measured %.2f:1 with nothing overlapping, below %.1f:1", ratio, Self.minimumContrastRatio))
            }
            if settled[element.key] == nil { settled[element.key] = screenName }
            return .measured(ratio)
        }
    }

    /// After an audit stop: every listed element that was unobstructed there and had no contrast issue is settled.
    /// Returns the keys newly settled at this stop.
    @discardableResult
    mutating func recordStop(
        _ screenName: String, listed: [(element: ScreenElement, placement: GlassPlacement)], flaggedKeys: Set<String>,
        clearBand: ClosedRange<CGFloat>, screen: CGRect, others: [ScreenElement]
    ) -> [ScreenElement] {
        var newlySettled: [ScreenElement] = []
        for (element, placement) in listed where !flaggedKeys.contains(element.key) {
            guard Self.isUnobstructed(element, placement: placement, clearBand: clearBand, screen: screen, others: others) else { continue }
            if settled[element.key] == nil {
                settled[element.key] = screenName
                newlySettled.append(element)
            }
        }
        return newlySettled
    }

    /// Settles `key` by an unobstructed audit made elsewhere (the default-size run, for the largest size).
    mutating func settle(_ key: String, by evidence: String) {
        if settled[key] == nil { settled[key] = evidence }
    }

    /// Excuses the test has not settled: each is an element whose contrast was never shown unobstructed.
    var unsettled: [(key: String, firstScreen: String)] {
        owed.filter { settled[$0.key] == nil }.map { (key: $0.key, firstScreen: $0.value) }.sorted { $0.key < $1.key }
    }
}

/// The contrast of the text in a crop of a screenshot, as a person sees it.
enum ScreenshotContrast {
    /// `pixels` is RGBA, 8 bits per channel, `width` × `height`. The most common colour (in 5-bit buckets) is the
    /// background; the pixel furthest from it in luminance is the text. Pixels inside `excluded` (pixel coordinates) are
    /// skipped. Callers measure only a crop that nothing else overlaps (`GlassContrastLedger.overlapping`), so the furthest
    /// pixel is the element's own glyph (or, with the glyph excluded, the rest of the element's own ink).
    static func ratio(pixels: [UInt8], width: Int, height: Int, excluded: [CGRect] = []) -> Double? {
        guard width > 0, height > 0, pixels.count >= width * height * 4 else { return nil }
        var offsets: [Int] = []
        offsets.reserveCapacity(width * height)
        for row in 0..<height {
            for column in 0..<width {
                let point = CGPoint(x: CGFloat(column) + 0.5, y: CGFloat(row) + 0.5)
                if excluded.contains(where: { $0.contains(point) }) { continue }
                offsets.append((row * width + column) * 4)
            }
        }
        guard !offsets.isEmpty else { return nil }
        func bucket(_ offset: Int) -> UInt32 {
            let red = UInt32(pixels[offset] >> 3)
            let green = UInt32(pixels[offset + 1] >> 3)
            let blue = UInt32(pixels[offset + 2] >> 3)
            return (red << 16) | (green << 8) | blue
        }
        var counts: [UInt32: Int] = [:]
        for offset in offsets { counts[bucket(offset), default: 0] += 1 }
        guard let backgroundKey = counts.max(by: { $0.value < $1.value })?.key,
              let backgroundOffset = offsets.first(where: { bucket($0) == backgroundKey }) else { return nil }
        let backgroundLuminance = luminance(pixels, backgroundOffset)
        var textLuminance = backgroundLuminance
        for offset in offsets {
            let candidate = luminance(pixels, offset)
            let candidateGap: Double = Swift.abs(candidate - backgroundLuminance)
            let currentGap: Double = Swift.abs(textLuminance - backgroundLuminance)
            if candidateGap > currentGap { textLuminance = candidate }
        }
        return contrast(backgroundLuminance, textLuminance)
    }

    static func contrast(_ first: Double, _ second: Double) -> Double {
        let lighter: Double = Swift.max(first, second)
        let darker: Double = Swift.min(first, second)
        return (lighter + 0.05) / (darker + 0.05)
    }

    static func luminance(_ pixels: [UInt8], _ offset: Int) -> Double {
        func linear(_ value: UInt8) -> Double {
            let channel = Double(value) / 255
            if channel <= 0.03928 { return channel / 12.92 }
            let base: Double = (channel + 0.055) / 1.055
            return pow(base, 2.4)
        }
        let red: Double = linear(pixels[offset])
        let green: Double = linear(pixels[offset + 1])
        let blue: Double = linear(pixels[offset + 2])
        return 0.2126 * red + 0.7152 * green + 0.0722 * blue
    }
}
