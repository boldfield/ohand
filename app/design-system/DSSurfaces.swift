import SwiftUI

/// `surface1`, radius 24, the card shadow.
struct DSCard<Content: View>: View {
    var padding: CGFloat = DSSpacing.md
    let content: Content

    init(padding: CGFloat = DSSpacing.md, @ViewBuilder content: () -> Content) {
        self.padding = padding
        self.content = content()
    }

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: DSRadius.card, style: .continuous)
                    .fill(DSColor.surface1.color)
                    .dsShadow(.card))
    }
}

/// A card of rows with its title above and one footer line below. There are no full-width dividers.
struct DSGroupedSection<Content: View>: View {
    var title: String?
    var footer: String?
    let content: Content

    init(title: String? = nil, footer: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.footer = footer
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            if let title {
                Text(title)
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                    .padding(.horizontal, DSSpacing.md)
            }
            VStack(spacing: 0) {
                content
            }
            .frame(maxWidth: .infinity)
            .background(
                RoundedRectangle(cornerRadius: DSRadius.card, style: .continuous)
                    .fill(DSColor.surface1.color)
                    .dsShadow(.card))
            if let footer {
                Text(footer)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .padding(.horizontal, DSSpacing.md)
            }
        }
    }
}

/// The 32 pt `tint` tile with an `accent` glyph at the start of a row.
struct DSIconTile: View {
    let systemName: String

    var body: some View {
        Image(systemName: systemName)
            .font(.system(size: 15, weight: .semibold))
            .foregroundColor(DSColor.accent.color)
            .frame(width: DSLayout.iconTileSide, height: DSLayout.iconTileSide)
            .background(
                RoundedRectangle(cornerRadius: DSRadius.iconTile, style: .continuous)
                    .fill(DSColor.tint.color))
            .accessibilityHidden(true)
    }
}

/// A row at least 52 pt high: optional icon tile, label, optional value and a chevron. With an `action` the row is a
/// button; without one it is a plain label, so a `NavigationLink` using it as its label takes the tap on the whole
/// row, padding included.
struct DSRow: View {
    let label: String
    var iconSystemName: String?
    var value: String?
    var showsChevron = true
    var action: (() -> Void)?

    /// True when the row is its own button. A row used as a link label has no action and must not be one, or the
    /// inner button would take the link's tap.
    var rendersAsButton: Bool { action != nil }

    var body: some View {
        if let action {
            Button(action: action) { content }
                .buttonStyle(.plain)
        } else {
            content
        }
    }

    private var content: some View {
        HStack(spacing: DSSpacing.sm) {
            if let iconSystemName {
                DSIconTile(systemName: iconSystemName)
            }
            Text(label)
                .font(DSFont.rowLabel)
                .foregroundColor(DSColor.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let value {
                Text(value)
                    .font(DSFont.rowLabel)
                    .foregroundColor(DSColor.inkMuted.color)
            }
            if showsChevron {
                Image(systemName: "chevron.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundColor(DSColor.inkFaint.color)
                    .accessibilityHidden(true)
            }
        }
        .padding(.horizontal, DSSpacing.md)
        .padding(.vertical, DSSpacing.xs)
        .frame(minHeight: DSLayout.rowMinimumHeight)
        .contentShape(Rectangle())
    }
}

/// A single-choice row: an `accent` checkmark when selected and an optional one-line detail.
struct DSChoiceRow: View {
    let label: String
    var detail: String?
    let isSelected: Bool
    var action: () -> Void = {}

    var body: some View {
        Button(action: action) {
            HStack(spacing: DSSpacing.sm) {
                VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                    Text(label)
                        .font(DSFont.rowLabel)
                        .foregroundColor(DSColor.ink.color)
                    if let detail {
                        Text(detail)
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.inkMuted.color)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "checkmark")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundColor(DSColor.accent.color)
                    .opacity(isSelected ? 1 : 0)
                    .accessibilityHidden(true)
            }
            .padding(.horizontal, DSSpacing.md)
            .padding(.vertical, DSSpacing.xs)
            .frame(minHeight: DSLayout.rowMinimumHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        // SwiftUI has no radio-button trait; a selected button is the closest public equivalent.
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

/// The system toggle tinted `accent`, in a row.
struct DSToggleRow: View {
    let label: String
    var iconSystemName: String?
    @Binding var isOn: Bool

    var body: some View {
        HStack(spacing: DSSpacing.sm) {
            if let iconSystemName {
                DSIconTile(systemName: iconSystemName)
            }
            Toggle(label, isOn: $isOn)
                .font(DSFont.rowLabel)
                .foregroundColor(DSColor.ink.color)
                .tint(DSColor.accent.color)
        }
        .padding(.horizontal, DSSpacing.md)
        .padding(.vertical, DSSpacing.xs)
        .frame(minHeight: DSLayout.rowMinimumHeight)
    }
}

/// A 34 pt capsule. The hit area is still 44 pt tall.
struct DSChip: View {
    let title: String
    var isSelected = false
    var action: () -> Void = {}

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(DSFont.chip)
                .foregroundColor((isSelected ? DSColor.onAccent : DSColor.ink).color)
                .padding(.horizontal, DSSpacing.md)
                .frame(height: DSLayout.chipHeight)
                .background(Capsule().fill((isSelected ? DSColor.accent : DSColor.surface2).color))
                .frame(minWidth: DSLayout.minimumTouchTarget, minHeight: DSLayout.minimumTouchTarget)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

/// The "Saved" confirmation: a `tint` capsule with an `accent` check disc and the floating shadow.
struct DSSavedPill: View {
    var body: some View {
        HStack(spacing: DSSpacing.xs) {
            Image(systemName: "checkmark")
                .font(.system(size: 11, weight: .bold))
                .foregroundColor(DSColor.onAccent.color)
                .frame(width: 22, height: 22)
                .background(Circle().fill(DSColor.accent.color))
                .accessibilityHidden(true)
            Text("Saved")
                .font(DSFont.chip)
                .foregroundColor(DSColor.ink.color)
        }
        .padding(.horizontal, DSSpacing.sm)
        .frame(minHeight: DSLayout.minimumTouchTarget)
        .background(Capsule().fill(DSColor.tint.color))
        .dsShadow(.floating)
        .accessibilityElement(children: .combine)
    }
}

#if DEBUG
#Preview("Surfaces") {
    DSPreviewPair {
        ScrollView {
            VStack(spacing: DSSpacing.md) {
                DSCard { Text("A saved thought").foregroundColor(DSColor.ink.color) }
                DSGroupedSection(title: "Processing", footer: "Applies to new captures.") {
                    DSRow(label: "Daily prompt", iconSystemName: "bell", value: "On")
                    DSChoiceRow(label: "On this phone", detail: "Nothing leaves the device", isSelected: true)
                    DSChoiceRow(label: "With my own server", isSelected: false)
                    DSToggleRow(label: "Daily prompt", iconSystemName: "bell", isOn: .constant(true))
                }
                HStack {
                    DSChip(title: "Note", isSelected: true)
                    DSChip(title: "Today")
                }
                DSSavedPill()
            }
        }
    }
}
#endif
