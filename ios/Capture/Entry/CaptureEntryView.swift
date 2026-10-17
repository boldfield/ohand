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
    /// How long the Saved pill stays before it fades.
    static let savedPillDuration: TimeInterval = 2
    /// The widest the composer grows at regular width.
    static let composerMaximumWidth: CGFloat = 500

    @ObservedObject var model: CaptureEntryModel
    var actionsAreDisabled = false
    var open: (String) -> Void = { _ in }
    var notice: String?

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
            .onChange(of: model.presentation) { _ in reportReady() }
            .onChange(of: scenePhase) { phase in
                if phase == .active { model.refreshProgress() }
            }
            .onChange(of: model.mode) { _ in focusComposerIfSilent() }
            .onReceive(model.$acknowledgment) { acknowledgment in
                guard let acknowledgment = acknowledgment, !acknowledgment.isSaving else { return }
                UIAccessibility.post(notification: .announcement, argument: model.message(for: acknowledgment))
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
        .padding(.horizontal, sideMargin)
        .padding(.top, DSSpacing.xs)
        .padding(.bottom, DSSpacing.xs)
    }

    private var bottomArea: some View {
        VStack(spacing: DSSpacing.xs) {
            if savedPillIsVisible {
                DSSavedPill()
                    .accessibilityIdentifier(Self.savedPillIdentifier)
                    .transition(.opacity)
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
        .accessibilityIdentifier(Self.composerIdentifier)
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

    /// One short line about the last capture or the recorder, if there is anything to say.
    private var shortMessage: String? {
        if let notice = model.voiceNotice { return notice }
        if model.protectedDataNotice != nil { return CaptureMessages.protectedDataNotice }
        switch model.recorderState {
        case .requestingPermission: return CaptureMessages.waitingForMicrophone
        case .finishing: return CaptureMessages.finishingRecording
        case .idle, .recording: break
        }
        return model.acknowledgment.flatMap(CaptureMessages.message(for:))
    }

    private func messageText(_ message: String, identifier: String) -> some View {
        Text(message)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(identifier)
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
