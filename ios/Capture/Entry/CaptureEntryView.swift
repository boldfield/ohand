import SwiftUI
import UIKit

/// The surface a system entry opens: voice or silent capture and the acknowledgment, nothing else. It has no history,
/// suggestions, dashboard or webview, and it is shown from the model alone.
struct CaptureEntryView: View {
    static let modePickerIdentifier = "capture-entry.mode"
    static let fieldIdentifier = "capture-entry.field"
    static let saveIdentifier = "capture-entry.save"
    static let recordIdentifier = "capture-entry.record"
    static let stopIdentifier = "capture-entry.stop"
    static let cancelIdentifier = "capture-entry.cancel"
    static let noticeIdentifier = "capture-entry.notice"

    @ObservedObject var model: CaptureEntryModel
    @Environment(\.scenePhase) private var scenePhase
    @FocusState private var fieldIsFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Picker("Capture mode", selection: Binding(get: { model.mode }, set: { model.switchMode(to: $0) })) {
                Text("Voice").tag(CaptureEntryMode.voice)
                Text("Silent").tag(CaptureEntryMode.silent)
            }
            .pickerStyle(.segmented)
            .disabled(!model.isIdle)
            .accessibilityIdentifier(Self.modePickerIdentifier)

            if let notice = model.protectedDataNotice {
                noticeText(notice)
            }
            if let message = model.acknowledgment.map({ model.message(for: $0) }) {
                CaptureAcknowledgmentView(message: message)
            }
            if let voiceNotice = model.voiceNotice {
                noticeText(voiceNotice)
            }

            switch model.mode {
            case .silent: silentCapture
            case .voice: voiceCapture
            }
        }
        .padding()
        .onAppear {
            model.refreshProgress()
            focusFieldIfSilent()
            reportReady()
        }
        .onChange(of: model.presentation) { _ in reportReady() }
        .onChange(of: scenePhase) { phase in
            if phase == .active { model.refreshProgress() }
        }
        .onChange(of: model.mode) { _ in focusFieldIfSilent() }
        .onReceive(model.$acknowledgment) { acknowledgment in
            guard let acknowledgment = acknowledgment, !acknowledgment.isSaving else { return }
            UIAccessibility.post(notification: .announcement, argument: model.message(for: acknowledgment))
        }
    }

    private var silentCapture: some View {
        VStack(alignment: .leading, spacing: 12) {
            TextEditor(text: Binding(get: { model.textModel.text }, set: { model.textModel.updateText($0) }))
                .font(.body)
                .focused($fieldIsFocused)
                .frame(minHeight: 80, maxHeight: .infinity)
                .accessibilityLabel("Capture text")
                .accessibilityIdentifier(Self.fieldIdentifier)
            Button(action: { model.saveText() }) {
                Text("Save").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(!model.textModel.canSave)
            .accessibilityIdentifier(Self.saveIdentifier)
        }
    }

    private var voiceCapture: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch model.recorderState {
            case .idle:
                Button(action: { model.startRecording() }) {
                    Text("Record").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier(Self.recordIdentifier)
            case .requestingPermission:
                Text("Waiting for microphone permission\u{2026}").font(.callout)
            case .recording:
                Text("Recording").font(.headline)
                Button(action: { model.stopRecording() }) {
                    Text("Stop and save").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier(Self.stopIdentifier)
                Button(action: { model.cancelRecording() }) {
                    Text("Cancel").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .accessibilityIdentifier(Self.cancelIdentifier)
            case .finishing:
                Text("Finishing\u{2026}").font(.callout)
            }
            Spacer(minLength: 0)
        }
    }

    private func noticeText(_ message: String) -> some View {
        Text(message)
            .font(.callout)
            .foregroundColor(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(Self.noticeIdentifier)
    }

    /// Runs after the focus request above has been queued, so the measurement ends once the field can take input.
    private func reportReady() {
        DispatchQueue.main.async { model.surfaceDidBecomeReady() }
    }

    private func focusFieldIfSilent() {
        guard model.mode == .silent else { return }
        DispatchQueue.main.async { fieldIsFocused = true }
    }
}
