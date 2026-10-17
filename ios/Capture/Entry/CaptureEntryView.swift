import SwiftUI
import UIKit

/// The composer home: a glass header, an empty body and the composer at the bottom. Voice or silent capture and the
/// acknowledgment are all it shows. It has no history, suggestions, dashboard or webview, and it is shown from the
/// model alone, so recents never appear here.
struct CaptureEntryView: View {
    static let noticeIdentifier = "capture-entry.notice"
    static let messageIdentifier = "capture-entry.message"
    static let composerIdentifier = "capture-entry.composer"
    static let savedPillIdentifier = "capture-entry.saved"
    static let headerIdentifier = "capture-entry.header"
    /// Reported while the composer shows that state (`rest`, `typing` or `recording`), so tests can tell them apart.
    static func composerStateIdentifier(_ state: String) -> String { "capture-entry.composer.\(state)" }
    /// Reported while the composer is disabled for an in-flight save.
    static let composerSavingIdentifier = "capture-entry.composer.saving"
    /// How long the Saved pill stays before it fades.
    static let savedPillDuration: TimeInterval = 2
    /// The widest the composer grows at regular width.
    static let composerMaximumWidth: CGFloat = 500

    @ObservedObject var model: CaptureEntryModel
    var actionsAreDisabled = false
    var open: (String) -> Void = { _ in }
    var notice: String?
    /// Test seam: receives where the header, composer and Saved pill (and Settings or Done) land on screen.
    var layoutReporter: (([String: CGRect]) -> Void)?

    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @FocusState private var composerIsFocused: Bool
    @State private var keyboardIsVisible = false
    @State private var savedPillIsVisible = false
    @State private var savedPillGeneration = 0

    var body: some View {
        Color.clear
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(DSColor.surface0.color.ignoresSafeArea())
            .safeAreaInset(edge: .top, spacing: 0) { header }
            .safeAreaInset(edge: .bottom, spacing: 0) { bottomArea }
            .onAppear {
                model.refreshProgress()
                focusComposerIfSilent()
                reportReady()
            }
            .onPreferenceChange(HomeLayoutKey.self) { frames in layoutReporter?(frames) }
            .onChange(of: model.presentation) { _ in reportReady() }
            .onChange(of: scenePhase) { phase in
                if phase == .active { model.refreshProgress() }
            }
            .onChange(of: model.mode) { _ in focusComposerIfSilent() }
            .onReceive(model.$acknowledgment) { acknowledgment in
                guard let acknowledgment = acknowledgment, !acknowledgment.isSaving else { return }
                let spoken = CaptureMessages.message(for: acknowledgment) ?? model.message(for: acknowledgment)
                UIAccessibility.post(notification: .announcement, argument: spoken)
                if acknowledgment.savedItemID != nil { showSavedPill() }
            }
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
                keyboardIsVisible = true
            }
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
                keyboardIsVisible = false
            }
    }

    private var isRegularWidth: Bool { horizontalSizeClass == .regular }

    private var sideMargin: CGFloat { DSSpacing.sideMargin(for: horizontalSizeClass) }

    private var header: some View {
        VStack(spacing: DSSpacing.xs) {
            HomeHeader(
                keyboardIsVisible: keyboardIsVisible,
                actionsAreDisabled: actionsAreDisabled,
                isRegularWidth: isRegularWidth,
                onSearch: { open(ManagementCatalog.searchEntryID) },
                onSettings: { open(ManagementCatalog.settingsEntryID) },
                onDone: dismissKeyboard)
            if let notice = notice {
                messageText(notice, identifier: AppRootView.noticeIdentifier)
            }
        }
        .reportHomeFrame(Self.headerIdentifier)
        .padding(.horizontal, sideMargin)
        .padding(.top, DSSpacing.xs)
        .padding(.bottom, DSSpacing.xs)
    }

    private var bottomArea: some View {
        VStack(spacing: DSSpacing.xs) {
            if savedPillIsVisible {
                DSSavedPill()
                    .accessibilityIdentifier(Self.savedPillIdentifier)
                    .reportHomeFrame(Self.savedPillIdentifier)
                    .transition(.opacity)
            }
            if let notice = standingNotice {
                messageText(notice, identifier: Self.noticeIdentifier)
            }
            if let message = shortMessage {
                messageText(message, identifier: Self.messageIdentifier)
            }
            composer
        }
        .frame(maxWidth: isRegularWidth ? Self.composerMaximumWidth : .infinity)
        .padding(.horizontal, sideMargin)
        .padding(.bottom, DSSpacing.xs)
        .frame(maxWidth: .infinity)
    }

    private var composer: some View {
        Group {
            if case .recording(_, let startedAt) = model.recorderState {
                TimelineView(.periodic(from: startedAt, by: 1)) { context in
                    composerView(state: .recording, elapsedText: CaptureMessages.elapsedText(since: startedAt, now: context.date))
                }
            } else {
                composerView(state: model.textModel.text.isEmpty ? .rest : .typing, elapsedText: "0:00")
            }
        }
        .disabled(model.textModel.isSaving)
        .onChange(of: model.textModel.isSaving) { isSaving in
            // Disabling the composer for the in-flight save drops focus; a save keeps the keyboard up.
            if !isSaving, keyboardIsVisible { DispatchQueue.main.async { composerIsFocused = true } }
        }
        .accessibilityIdentifier(Self.composerIdentifier)
        .reportHomeFrame(Self.composerIdentifier)
        .reportHomeFrame(Self.composerStateIdentifier(composerStateName))
        .background {
            if model.textModel.isSaving { Color.clear.reportHomeFrame(Self.composerSavingIdentifier) }
        }
    }

    private var composerStateName: String {
        if case .recording = model.recorderState { return "recording" }
        return model.textModel.text.isEmpty ? "rest" : "typing"
    }

    private func composerView(state: DSComposerState, elapsedText: String) -> some View {
        DSComposer(
            state: state,
            text: Binding(get: { model.textModel.text }, set: { model.textModel.updateText($0) }),
            elapsedText: elapsedText,
            onMic: { model.startRecording() },
            onSave: { if model.textModel.canSave { model.saveText() } },
            onDiscard: { model.cancelRecording() },
            onStop: { model.stopRecording() })
            .focused($composerIsFocused)
    }

    /// Notices that stay beside the outcome: they never hide what happened to the last capture, or each other.
    private var standingNotice: String? {
        Self.standingNotice(voiceNotice: model.voiceNotice, protectedDataNoticeIsActive: model.protectedDataNotice != nil)
    }

    static func standingNotice(voiceNotice: String?, protectedDataNoticeIsActive: Bool) -> String? {
        let present = [voiceNotice, protectedDataNoticeIsActive ? CaptureMessages.protectedDataNotice : nil].compactMap { $0 }
        return present.isEmpty ? nil : present.joined(separator: " ")
    }

    /// One short line about the last capture or the recorder, if there is anything to say.
    private var shortMessage: String? {
        switch model.recorderState {
        case .requestingPermission: return CaptureMessages.waitingForMicrophone
        case .finishing: return CaptureMessages.finishingRecording
        case .idle, .recording: break
        }
        return model.acknowledgment.flatMap { CaptureMessages.message(for: $0) }
    }

    private func messageText(_ message: String, identifier: String) -> some View {
        Text(message)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(identifier)
            .reportHomeFrame(identifier)
    }

    private func showSavedPill() {
        savedPillGeneration += 1
        let generation = savedPillGeneration
        withAnimation { savedPillIsVisible = true }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.savedPillDuration) {
            guard generation == savedPillGeneration else { return }
            withAnimation { savedPillIsVisible = false }
        }
    }

    private func dismissKeyboard() {
        composerIsFocused = false
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    /// Runs after the focus request below has been queued, so the measurement ends once the field can take input.
    private func reportReady() {
        DispatchQueue.main.async { model.surfaceDidBecomeReady() }
    }

    private func focusComposerIfSilent() {
        guard model.mode == .silent else { return }
        DispatchQueue.main.async { composerIsFocused = true }
    }
}
