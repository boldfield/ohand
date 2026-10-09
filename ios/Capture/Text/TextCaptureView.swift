import SwiftUI
import UIKit

/// Window-space frames of the parts of the capture surface, reported so layout under large text and a visible
/// keyboard can be tested. A part that is not on screen reports `.zero`.
struct TextCaptureLayoutSnapshot: Equatable {
    var editor: CGRect = .zero
    var saveButton: CGRect = .zero
    var status: CGRect = .zero
}

private struct TextCaptureLayoutKey: PreferenceKey {
    static var defaultValue = TextCaptureLayoutSnapshot()

    static func reduce(value: inout TextCaptureLayoutSnapshot, nextValue: () -> TextCaptureLayoutSnapshot) {
        let next = nextValue()
        if next.editor != .zero { value.editor = next.editor }
        if next.saveButton != .zero { value.saveButton = next.saveButton }
        if next.status != .zero { value.status = next.status }
    }
}

private func layoutSnapshot(
    _ part: WritableKeyPath<TextCaptureLayoutSnapshot, CGRect>, frame: CGRect
) -> TextCaptureLayoutSnapshot {
    var snapshot = TextCaptureLayoutSnapshot()
    snapshot[keyPath: part] = frame
    return snapshot
}

private extension View {
    func reportFrame(_ part: WritableKeyPath<TextCaptureLayoutSnapshot, CGRect>) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(key: TextCaptureLayoutKey.self, value: layoutSnapshot(part, frame: proxy.frame(in: .global)))
        })
    }
}

/// The direct native capture surface: a focused text field and one save action, with the result stated honestly.
/// It needs no dashboard, tag, project choice or other setup first.
struct TextCaptureView: View {
    static let fieldIdentifier = "text-capture.field"
    static let saveIdentifier = "text-capture.save"
    static let statusIdentifier = "text-capture.status"

    @ObservedObject var model: TextCaptureModel
    var layoutReporter: ((TextCaptureLayoutSnapshot) -> Void)?

    @FocusState private var fieldIsFocused: Bool

    init(model: TextCaptureModel, layoutReporter: ((TextCaptureLayoutSnapshot) -> Void)? = nil) {
        self.model = model
        self.layoutReporter = layoutReporter
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let message = model.status.message {
                TextCaptureStatusView(message: message)
            }
            TextEditor(text: Binding(get: { model.text }, set: { model.updateText($0) }))
                .font(.body)
                .focused($fieldIsFocused)
                .frame(minHeight: 80, maxHeight: .infinity)
                .accessibilityLabel("Capture text")
                .accessibilityIdentifier(Self.fieldIdentifier)
                .reportFrame(\.editor)
        }
        .padding()
        .safeAreaInset(edge: .bottom) {
            Button(action: { model.save() }) {
                Text("Save").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(!model.canSave)
            .accessibilityIdentifier(Self.saveIdentifier)
            .reportFrame(\.saveButton)
            .padding(.horizontal)
            .padding(.vertical, 8)
            .background(.bar)
        }
        .onAppear {
            DispatchQueue.main.async { fieldIsFocused = true }
        }
        .onReceive(model.$status) { status in
            if let message = status.message, status != .saving {
                UIAccessibility.post(notification: .announcement, argument: message)
            }
        }
        .onPreferenceChange(TextCaptureLayoutKey.self) { snapshot in
            layoutReporter?(snapshot)
        }
    }
}

private struct TextCaptureStatusView: View {
    let message: String

    @Environment(\.sizeCategory) private var sizeCategory

    var body: some View {
        Group {
            if sizeCategory.isAccessibilityCategory {
                ScrollView { text }.frame(maxHeight: 140)
            } else {
                text
            }
        }
        .reportFrame(\.status)
    }

    private var text: some View {
        Text(message)
            .font(.callout)
            .foregroundColor(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(TextCaptureView.statusIdentifier)
    }
}
