import SwiftUI

/// The three visual states of the composer. The caller decides which one shows; the composer does no capture.
enum DSComposerState {
    case rest
    case typing
    case recording
}

struct DSComposer: View {
    let state: DSComposerState
    @Binding var text: String
    var elapsedText = "0:00"
    var levels: [CGFloat] = [0.3, 0.6, 0.9, 0.5, 0.7, 0.4, 0.8, 0.35]
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
                }
                TextField("", text: $text, axis: .vertical)
                    .font(DSFont.capture)
                    .foregroundColor(DSColor.ink.color)
                    .tint(DSColor.accent.color)
                    .lineLimit(1...8)
                    .accessibilityLabel("What's on your mind?")
            }
            .padding(.leading, DSSpacing.xs)
            .padding(.vertical, DSSpacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
            if state == .typing {
                Button(action: onMic) {
                    micGlyph(foreground: DSColor.accent, fill: DSColor.tint)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Record")
                DSPillButton(title: "Save", style: .primary, action: onSave)
            } else {
                Button(action: onMic) {
                    micGlyph(foreground: DSColor.onAccent, fill: DSColor.accent)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Record")
            }
        }
    }

    private func micGlyph(foreground: DSColor, fill: DSColor) -> some View {
        Image(systemName: "mic.fill")
            .font(.system(size: 20, weight: .semibold))
            .foregroundColor(foreground.color)
            .frame(width: DSLayout.composerMicSide, height: DSLayout.composerMicSide)
            .background(Circle().fill(fill.color))
            .contentShape(Circle())
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
                Spacer()
                Color.clear.frame(width: DSLayout.minimumTouchTarget, height: DSLayout.minimumTouchTarget)
                    .accessibilityHidden(true)
            }
        }
    }
}

#if DEBUG
#Preview("Composer") {
    DSPreviewPair {
        VStack(spacing: DSSpacing.md) {
            DSComposer(state: .rest, text: .constant(""))
            DSComposer(state: .typing, text: .constant("Call the dentist about the crown"))
            DSComposer(state: .recording, text: .constant(""), elapsedText: "0:12")
        }
    }
}
#endif
