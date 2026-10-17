import SwiftUI

/// The home's composer. It draws `DSComposer`'s three states with the same tokens, and adds what the home needs and the
/// design-system composer has no seam for: Record and Save can be disabled on their own, so the text field keeps focus
/// (and the keyboard stays up) while a save or the recorder is busy. Each control it actually renders reports its frame,
/// so a hosted test checks the drawn output rather than a parallel computation.
struct HomeComposer: View {
    static let placeholderIdentifier = "home.composer.placeholder"
    static let fieldIdentifier = "home.composer.field"
    static let recordIdentifier = "home.composer.record"
    static let saveIdentifier = "home.composer.save"
    static let discardIdentifier = "home.composer.discard"
    static let stopIdentifier = "home.composer.stop"
    /// Reported in place of a control's own identifier while that control is disabled.
    static func disabledIdentifier(_ identifier: String) -> String { "\(identifier).disabled" }
    /// Reported while the text field is focused.
    static let focusedIdentifier = "home.composer.focused"

    let state: DSComposerState
    @Binding var text: String
    var elapsedText = "0:00"
    var levels: [CGFloat] = [0.3, 0.6, 0.9, 0.5, 0.7, 0.4, 0.8, 0.35]
    /// Record and Save. The text field is never disabled.
    var entryActionsAreEnabled = true
    var focus: FocusState<Bool>.Binding
    var onMic: () -> Void = {}
    var onSave: () -> Void = {}
    var onDiscard: () -> Void = {}
    var onStop: () -> Void = {}

    var body: some View {
        Group {
            if state == .recording {
                recordingPanel
            } else {
                entryRow
            }
        }
        .padding(DSSpacing.sm)
        .frame(maxWidth: .infinity)
        .dsGlass(in: RoundedRectangle(cornerRadius: DSRadius.composer, style: .continuous))
        .dsShadow(.floating)
    }

    private var entryRow: some View {
        HStack(alignment: .bottom, spacing: DSSpacing.xs) {
            ZStack(alignment: .topLeading) {
                if text.isEmpty {
                    Text("What's on your mind?")
                        .font(DSFont.capturePlaceholder)
                        .foregroundColor(DSColor.inkFaint.color)
                        .accessibilityHidden(true)
                        .reportHomeFrame(Self.placeholderIdentifier)
                }
                TextField("", text: $text, axis: .vertical)
                    .font(DSFont.capture)
                    .foregroundColor(DSColor.ink.color)
                    .tint(DSColor.accent.color)
                    .lineLimit(1...8)
                    .accessibilityLabel("What's on your mind?")
                    .focused(focus)
                    .modifier(ReportsEnabledFrame(identifier: Self.fieldIdentifier))
                    .background {
                        if focus.wrappedValue { Color.clear.reportHomeFrame(Self.focusedIdentifier) }
                    }
            }
            .padding(.leading, DSSpacing.xs)
            .padding(.vertical, DSSpacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
            if state == .typing {
                micButton(foreground: DSColor.accent, fill: DSColor.tint)
                DSPillButton(title: "Save", style: .primary, action: onSave)
                    .modifier(ReportsEnabledFrame(identifier: Self.saveIdentifier))
                    .disabled(!entryActionsAreEnabled)
            } else {
                micButton(foreground: DSColor.onAccent, fill: DSColor.accent)
            }
        }
    }

    private func micButton(foreground: DSColor, fill: DSColor) -> some View {
        Button(action: onMic) {
            Image(systemName: "mic.fill")
                .font(.system(size: 20, weight: .semibold))
                .foregroundColor((entryActionsAreEnabled ? foreground : DSColor.inkFaint).color)
                .frame(width: DSLayout.composerMicSide, height: DSLayout.composerMicSide)
                .background(Circle().fill((entryActionsAreEnabled ? fill : DSColor.surface2).color))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Record")
        .modifier(ReportsEnabledFrame(identifier: Self.recordIdentifier))
        .disabled(!entryActionsAreEnabled)
    }

    private var recordingPanel: some View {
        VStack(spacing: DSSpacing.sm) {
            HStack(spacing: DSSpacing.xs) {
                Circle().fill(DSColor.signalRecord.color).frame(width: 10, height: 10)
                Text("Recording")
                    .font(DSFont.groupHeading)
                    .foregroundColor(DSColor.ink.color)
            }
            .accessibilityElement(children: .combine)
            Text(elapsedText)
                .font(DSFont.numeral)
                .foregroundColor(DSColor.ink.color)
                .minimumScaleFactor(0.5)
                .lineLimit(1)
            HStack(spacing: 3) {
                ForEach(levels.indices, id: \.self) { index in
                    Capsule()
                        .fill(DSColor.accent.color)
                        .frame(width: 4, height: 28 * levels[index])
                }
            }
            .frame(height: 28)
            .accessibilityHidden(true)
            HStack {
                DSPillButton(title: "Discard", style: .destructive, action: onDiscard)
                    .modifier(ReportsEnabledFrame(identifier: Self.discardIdentifier))
                Spacer()
                Button(action: onStop) {
                    Image(systemName: "stop.fill")
                        .font(.system(size: 26, weight: .semibold))
                        .foregroundColor(DSColor.onAccent.color)
                        .frame(width: DSLayout.stopButtonSide, height: DSLayout.stopButtonSide)
                        .background(Circle().fill(DSColor.signalRecord.color))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Stop recording")
                .modifier(ReportsEnabledFrame(identifier: Self.stopIdentifier))
                Spacer()
                Color.clear.frame(width: DSLayout.minimumTouchTarget, height: DSLayout.minimumTouchTarget)
                    .accessibilityHidden(true)
            }
        }
    }
}

/// Reports the view's frame under its identifier, or under the disabled identifier when the environment that actually
/// reaches the control has disabled it.
private struct ReportsEnabledFrame: ViewModifier {
    let identifier: String
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.reportHomeFrame(isEnabled ? identifier : HomeComposer.disabledIdentifier(identifier))
    }
}
