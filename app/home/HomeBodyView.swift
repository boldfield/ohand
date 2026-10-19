import SwiftUI

/// The body of the composer home: recent captures when reads are authenticated, placeholder cards while a quiet attempt
/// is in flight, and the locked card otherwise. Nothing derived from stored history is drawn outside the unlocked state.
struct HomeBodyView: View {
    static let listIdentifier = "home.recents.list"
    static let cardIdentifierPrefix = "home.recents.card."
    static let placeholderIdentifier = "home.recents.placeholder"
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

    @ObservedObject var model: HomeRecentsModel
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass

    var body: some View {
        ScrollView {
            content
                .padding(.horizontal, DSSpacing.sideMargin(for: horizontalSizeClass))
                .padding(.vertical, DSSpacing.sm)
                .frame(maxWidth: .infinity)
        }
    }

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
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                DSIconTile(systemName: "lock")
                Text(Self.lockedTitle)
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                Text(Self.lockedDetail)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                DSPillButton(title: Self.unlockTitle, style: .primary, action: { model.unlock() })
                    .accessibilityIdentifier(Self.unlockButtonIdentifier)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.lockedCardIdentifier)
    }

    private var placeholders: some View {
        LazyVGrid(columns: columns, spacing: DSSpacing.sm) {
            ForEach(0..<Self.placeholderCount, id: \.self) { _ in
                DSCard {
                    VStack(alignment: .leading, spacing: DSSpacing.xs) {
                        placeholderBar(width: 96)
                        placeholderBar(width: nil)
                        placeholderBar(width: 160)
                    }
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityIdentifier(Self.placeholderIdentifier)
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
                DSPillButton(title: "Try again", style: .secondary, action: { reload() })
                    .accessibilityIdentifier(Self.retryIdentifier)
            }
        }
        .accessibilityIdentifier(Self.listIdentifier)
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
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.xs) {
                HStack(spacing: DSSpacing.xs) {
                    Circle()
                        .fill(Self.dotColor(forItemType: entry.itemType).color)
                        .frame(width: 8, height: 8)
                        .accessibilityHidden(true)
                    Text(Self.caption(for: entry))
                        .font(DSFont.groupHeading)
                        .foregroundColor(DSColor.inkMuted.color)
                }
                Text(Self.text(for: entry))
                    .font(DSFont.body)
                    .foregroundColor(DSColor.ink.color)
                    .lineLimit(4)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(Self.cardIdentifierPrefix + entry.id)
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
