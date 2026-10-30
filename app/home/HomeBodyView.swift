import SwiftUI

/// Where a part of the home body landed and the words it shows, so a hosted test can see what is on screen. A button
/// also carries the action its tap runs; the action is not compared.
struct HomeRenderedElement: Equatable {
    let frame: CGRect
    let text: String?
    let tap: (() -> Void)?

    static func == (lhs: HomeRenderedElement, rhs: HomeRenderedElement) -> Bool {
        lhs.frame == rhs.frame && lhs.text == rhs.text && (lhs.tap == nil) == (rhs.tap == nil)
    }
}

struct HomeRenderedKey: PreferenceKey {
    static var defaultValue: [String: HomeRenderedElement] = [:]

    static func reduce(value: inout [String: HomeRenderedElement], nextValue: () -> [String: HomeRenderedElement]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

extension View {
    /// Reports this part of the home body to a hosted test: where it is, what it says and, for a button, its action.
    func reportHomeRendered(_ identifier: String, text: String?, tap: (() -> Void)? = nil) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(
                key: HomeRenderedKey.self,
                value: [identifier: HomeRenderedElement(frame: proxy.frame(in: .global), text: text, tap: tap)])
        })
    }
}

/// The body of the composer home: recent captures when reads are authenticated, placeholder cards while a quiet attempt
/// is in flight, and the locked card otherwise. Nothing derived from stored history is drawn outside the unlocked state.
struct HomeBodyView: View {
    static let listIdentifier = "home.recents.list"
    static let cardIdentifierPrefix = "home.recents.card."
    static let placeholderIdentifier = "home.recents.placeholder"
    static let placeholderCardIdentifierPrefix = "home.recents.placeholder."
    static let cardCaptionIdentifierPrefix = "home.recents.caption."
    static let cardNeedsTimeIdentifierPrefix = "home.recents.needs-time."
    static let needsTimeLabel = "Needs a time"
    static let cardDotIdentifierPrefix = "home.recents.dot."
    static let lockedCardIdentifier = "home.recents.locked"
    static let unlockButtonIdentifier = "home.recents.unlock"
    static let todayHeadingIdentifier = "home.recents.today"
    static let earlierHeadingIdentifier = "home.recents.earlier"
    static let retryIdentifier = "home.recents.retry"

    static let lockedTitle = "Your captures are locked"
    static let lockedDetail = "You can still save new ones. Unlock to see what you saved before."
    static let unlockTitle = "Unlock"
    static let sortingLabel = "Just now \u{00B7} sorting\u{2026}"
    static let unsortedTypeTitle = "Not yet sorted"
    static let placeholderCount = 3
    static let lockedAnnouncement = lockedTitle + ". " + unlockTitle + " to see what you saved before."
    static let needsTimeAnnouncement = "A reminder needs a time."

    @ObservedObject var model: HomeRecentsModel
    /// Opens the item a tapped card shows. The assembly supplies it, so this folder never reaches the navigation model.
    var openItem: (String) -> Void = { _ in }
    /// Test seam: receives every reported part of the body each time it is drawn.
    var renderReporter: (([String: HomeRenderedElement]) -> Void)?
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass

    var body: some View {
        ScrollView {
            content
                .padding(.horizontal, DSSpacing.sideMargin(for: horizontalSizeClass))
                .padding(.vertical, DSSpacing.sm)
                .frame(maxWidth: .infinity)
        }
        .onPreferenceChange(HomeRenderedKey.self) { elements in renderReporter?(elements) }
        .onAppear {
            if model.bodyState == .locked { AccessibilityAnnouncer.announce(Self.lockedAnnouncement) }
            if needsTimeCount > 0 { AccessibilityAnnouncer.announce(Self.needsTimeAnnouncement) }
        }
        .onChange(of: model.bodyState) { state in
            if state == .locked { AccessibilityAnnouncer.announce(Self.lockedAnnouncement) }
        }
        .onChange(of: needsTimeCount) { count in
            if count > 0 { AccessibilityAnnouncer.announce(Self.needsTimeAnnouncement) }
        }
    }

    /// How many shown notes have a reminder that needs a time; a rise is announced.
    private var needsTimeCount: Int { model.entries.filter(\.needsTime).count }

    private var isRegularWidth: Bool { horizontalSizeClass == .regular }

    @ViewBuilder private var content: some View {
        switch model.bodyState {
        case .unlocked: recents
        case .unlocking: placeholders
        case .locked: lockedCard
        }
    }

    // MARK: Locked and unlocking

    private var lockedCard: some View {
        let unlock = { model.unlock() }
        return DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                DSIconTile(systemName: "lock")
                Text(Self.lockedTitle)
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                Text(Self.lockedDetail)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                DSPillButton(title: Self.unlockTitle, style: .primary, action: unlock)
                    .accessibilityIdentifier(Self.unlockButtonIdentifier)
                    .reportHomeRendered(Self.unlockButtonIdentifier, text: Self.unlockTitle, tap: unlock)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.lockedCardIdentifier)
        .reportHomeRendered(Self.lockedCardIdentifier, text: Self.lockedTitle + "\n" + Self.lockedDetail)
    }

    private var placeholders: some View {
        LazyVGrid(columns: columns, spacing: DSSpacing.sm) {
            ForEach(0..<Self.placeholderCount, id: \.self) { index in
                DSCard {
                    VStack(alignment: .leading, spacing: DSSpacing.xs) {
                        placeholderBar(width: 96)
                        placeholderBar(width: nil)
                        placeholderBar(width: 160)
                    }
                }
                .reportHomeRendered(Self.placeholderCardIdentifierPrefix + String(index), text: nil)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityIdentifier(Self.placeholderIdentifier)
        .reportHomeRendered(Self.placeholderIdentifier, text: nil)
    }

    private func placeholderBar(width: CGFloat?) -> some View {
        RoundedRectangle(cornerRadius: DSRadius.chip, style: .continuous)
            .fill(DSColor.surface2.color)
            .frame(width: width, height: 14)
            .frame(maxWidth: width == nil ? .infinity : nil, alignment: .leading)
    }

    // MARK: Recents

    private var columns: [GridItem] {
        let count = isRegularWidth ? 2 : 1
        return Array(repeating: GridItem(.flexible(), spacing: DSSpacing.sm, alignment: .top), count: count)
    }

    private var recents: some View {
        let entries = model.entries
        let today = entries.filter { model.section(for: $0) == .today }
        let earlier = entries.filter { model.section(for: $0) == .earlier }
        return VStack(alignment: .leading, spacing: DSSpacing.sm) {
            if !today.isEmpty {
                heading("Today", identifier: Self.todayHeadingIdentifier, large: isRegularWidth)
                cards(today)
            }
            if !earlier.isEmpty {
                heading("Earlier", identifier: Self.earlierHeadingIdentifier, large: false)
                cards(earlier)
            }
            if model.loadFailed {
                DSPillButton(title: "Try again", style: .secondary, action: reload)
                    .accessibilityIdentifier(Self.retryIdentifier)
                    .reportHomeRendered(Self.retryIdentifier, text: "Try again", tap: reload)
            }
        }
        .accessibilityIdentifier(Self.listIdentifier)
        .reportHomeRendered(Self.listIdentifier, text: nil)
    }

    private func reload() {
        if model.canLoadMore { model.loadMore() } else { model.loadFirstPage() }
    }

    private func heading(_ title: String, identifier: String, large: Bool) -> some View {
        Text(title)
            .font(large ? DSFont.displayXL : DSFont.groupHeading)
            .foregroundColor(large ? DSColor.ink.color : DSColor.inkMuted.color)
            .accessibilityAddTraits(.isHeader)
            .accessibilityIdentifier(identifier)
            .reportHomeRendered(identifier, text: title)
    }

    private func cards(_ entries: [HomeRecentEntry]) -> some View {
        LazyVGrid(columns: columns, spacing: DSSpacing.sm) {
            ForEach(entries) { entry in
                card(for: entry)
                    .onAppear { model.loadMoreIfNeeded(afterAppearing: entry.id) }
            }
        }
    }

    private func card(for entry: HomeRecentEntry) -> some View {
        let caption = Self.caption(for: entry, now: model.currentDate)
        let open = { openIfStillShown(entry.id) }
        return Button(action: open) { cardContent(for: entry, caption: caption) }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isButton)
            .accessibilityIdentifier(Self.cardIdentifierPrefix + entry.id)
            .reportHomeRendered(Self.cardIdentifierPrefix + entry.id, text: Self.text(for: entry), tap: open)
    }

    /// A card opens only while the list is unlocked and still holds it, so a relock that cleared the list wins.
    private func openIfStillShown(_ itemID: String) {
        guard model.bodyState == .unlocked, model.entries.contains(where: { $0.id == itemID }) else { return }
        openItem(itemID)
    }

    private func cardContent(for entry: HomeRecentEntry, caption: String) -> some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.xs) {
                HStack(spacing: DSSpacing.xs) {
                    Circle()
                        .fill(Self.dotColor(forItemType: entry.itemType).color)
                        .frame(width: 8, height: 8)
                        .accessibilityHidden(true)
                        .reportHomeRendered(Self.cardDotIdentifierPrefix + entry.id, text: entry.itemType)
                    Text(caption)
                        .font(DSFont.groupHeading)
                        .foregroundColor(DSColor.inkMuted.color)
                        .reportHomeRendered(Self.cardCaptionIdentifierPrefix + entry.id, text: caption)
                }
                Text(Self.text(for: entry))
                    .font(DSFont.body)
                    .foregroundColor(DSColor.ink.color)
                    .lineLimit(4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if entry.needsTime {
                    HStack(spacing: DSSpacing.xxs) {
                        Image(systemName: "clock.badge.questionmark")
                            .accessibilityHidden(true)
                        Text(Self.needsTimeLabel)
                    }
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(Self.needsTimeLabel)
                    .reportHomeRendered(Self.cardNeedsTimeIdentifierPrefix + entry.id, text: Self.needsTimeLabel)
                }
            }
        }
        .contentShape(Rectangle())
    }

    // MARK: Wording

    static func caption(for entry: HomeRecentEntry, now: Date = Date()) -> String {
        if entry.isSorting { return sortingLabel }
        return "\(typeTitle(for: entry.itemType)) \u{00B7} \(relativeDate(entry.capturedAt, now: now))"
    }

    static func typeTitle(for itemType: String?) -> String {
        guard let itemType else { return unsortedTypeTitle }
        return RetrievalItemType(rawValue: itemType)?.title ?? unsortedTypeTitle
    }

    static func text(for entry: HomeRecentEntry) -> String {
        if let text = entry.text, !text.isEmpty { return text }
        return entry.isVoice ? "Voice capture" : "Saved"
    }

    static func relativeDate(_ date: Date, now: Date) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        formatter.dateTimeStyle = .named
        return formatter.localizedString(for: date, relativeTo: now)
    }

    static func dotColor(forItemType itemType: String?) -> DSColor {
        switch itemType.flatMap(RetrievalItemType.init(rawValue:)) {
        case .some(.broadIntention): return .inkMuted
        case .some(.idea): return .signalWarning
        case .some(.note): return .accent
        case .some(.action): return .signalSuccess
        case .none: return .inkFaint
        }
    }
}
