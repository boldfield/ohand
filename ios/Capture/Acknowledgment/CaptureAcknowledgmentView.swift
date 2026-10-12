import SwiftUI

/// The statement of what happened to the last capture. Words only, no suggestions or prompts.
struct CaptureAcknowledgmentView: View {
    static let identifier = "capture-entry.acknowledgment"

    let message: String

    @Environment(\.sizeCategory) private var sizeCategory

    var body: some View {
        if sizeCategory.isAccessibilityCategory {
            ScrollView { text }.frame(maxHeight: 160)
        } else {
            text
        }
    }

    private var text: some View {
        Text(message)
            .font(.callout)
            .foregroundColor(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(Self.identifier)
    }
}
