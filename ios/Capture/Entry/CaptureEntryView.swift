import SwiftUI
import UIKit

/// The composer home: a glass header, a body slot and the composer at the bottom. The slot is empty unless the host
/// injects one (the home's recent captures and lock states); the entry itself is shown from the model alone, so it never
/// reads history, and it has no suggestions, dashboard or webview.
struct CaptureEntryView: View {
    static let noticeIdentifier = "capture-entry.notice"
    static let messageIdentifier = "capture-entry.message"
    static let composerIdentifier = "capture-entry.composer"
    static let savedPillIdentifier = "capture-entry.saved"
    static let headerIdentifier = "capture-entry.header"
    /// Reported while the composer shows that state (`rest`, `typing` or `recording`), so tests can tell them apart.
    static func composerStateIdentifier(_ state: String) -> String { "capture-entry.composer.\(state)" }
    /// How long the Saved pill stays before it fades.
    static let savedPillDuration: TimeInterval = 2
    /// The widest the composer grows at regular width.
    static let composerMaximumWidth: CGFloat = 500

    @ObservedObject var model: CaptureEntryModel
    var actionsAreDisabled = false
    var open: (String) -> Void = { _ in }
    var notice: String?
    /// The home body between the header and the composer. Empty by default.
    var homeBody: () -> AnyView = { AnyView(Color.clear) }
    /// Test seam: receives where the header, composer and Saved pill (and Settings or Done) land on screen.
    var layoutReporter: (([String: CGRect]) -> Void)?

    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @FocusState private var composerIsFocused: Bool
    @State private var keyboardIsVisible = false
    @State private var savedPillIsVisible = false
    @State private var savedPillGeneration = 0
    /// The save state the pill last reacted to; nil until the first acknowledgment publication has been seen.
    @State private var observedSave: ObservedSave?

    var body: some View {
        homeBody()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(DSColor.surface0.color.ignoresSafeArea())
            .safeAreaInset(edge: .top, spacing: 0) { header }
            .safeAreaInset(edge: .bottom, spacing: 0) { bottomArea }
            .onAppear {
                if observedSave == nil { observedSave = ObservedSave(save: model.acknowledgment?.save) }
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
            .onChange(of: model.isRecording) { isRecording in
                if isRecording {
                    UIAccessibility.post(notification: .announcement, argument: "Recording started.")
                }
            }
            .onReceive(model.$acknowledgment) { acknowledgment in
                updateSavedPill(for: acknowledgment?.save)
                guard let acknowledgment = acknowledgment, !acknowledgment.isSaving else { return }
                let spoken = CaptureMessages.message(for: acknowledgment) ?? model.message(for: acknowledgment)
                UIAccessibility.post(notification: .announcement, argument: spoken)
            }
            // A voice attempt that fails to start leaves the acknowledgment alone, so its notice hides the pill.
            .onReceive(model.$voiceNotice) { voiceNotice in
                if voiceNotice != nil { hideSavedPill() }
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
        let state = composerState
        return Group {
            if case .recording(_, let startedAt) = model.recorderState {
                TimelineView(.periodic(from: startedAt, by: 1)) { context in
                    composerView(state: state, elapsedText: CaptureMessages.elapsedText(since: startedAt, now: context.date))
                }
            } else {
                composerView(state: state, elapsedText: "0:00")
            }
        }
        // The composer's frame is reported under this identifier, but the group carries no accessibility identifier of
        // its own: SwiftUI hands a container's identifier to every element inside it, which would hide the field's and
        // the buttons' own identifiers from the screenshot walkthrough.
        .reportHomeFrame(Self.composerIdentifier)
        .reportHomeFrame(Self.composerStateIdentifier(Self.name(of: state)))
    }

    /// The one place the composer's state is chosen; the same value is drawn and reported.
    private var composerState: DSComposerState {
        if case .recording = model.recorderState { return .recording }
        return model.textModel.text.isEmpty ? .rest : .typing
    }

    static func name(of state: DSComposerState) -> String {
        switch state {
        case .rest: return "rest"
        case .typing: return "typing"
        case .recording: return "recording"
        }
    }

    /// Record and Save do nothing while a text save is in flight or the recorder is starting or finishing, so they are
    /// shown disabled then. The text field stays enabled and focused, so a save keeps the keyboard up.
    private var entryActionsAreEnabled: Bool {
        guard !model.textModel.isSaving else { return false }
        switch model.recorderState {
        case .idle, .recording: return true
        case .requestingPermission, .finishing: return false
        }
    }

    private func composerView(state: DSComposerState, elapsedText: String) -> some View {
        HomeComposer(
            state: state,
            text: Binding(get: { model.textModel.text }, set: { model.textModel.updateText($0) }),
            elapsedText: elapsedText,
            entryActionsAreEnabled: entryActionsAreEnabled,
            saveIsEnabled: model.textModel.canSave,
            focus: $composerIsFocused,
            onMic: {
                guard entryActionsAreEnabled else { return }
                hideSavedPill()
                model.startRecording()
            },
            onSave: { if entryActionsAreEnabled, model.textModel.canSave { model.saveText() } },
            onDiscard: { model.cancelRecording() },
            onStop: { model.stopRecording() })
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

    struct ObservedSave: Equatable {
        let save: CaptureSaveState?
    }

    enum SavedPillChange: Equatable {
        case show
        case hide
        case keep
    }

    /// The pill marks one save: it shows when a capture newly becomes saved, not when the same saved acknowledgment is
    /// republished by a progress or foreground refresh, and it goes as soon as the next attempt or outcome begins.
    /// What is already on screen when the home appears is not a new save.
    static func savedPillChange(from previous: ObservedSave?, to current: CaptureSaveState?) -> SavedPillChange {
        guard let previous = previous else { return .keep }
        if previous.save == current { return .keep }
        if case .saved = current { return .show }
        return .hide
    }

    private func updateSavedPill(for save: CaptureSaveState?) {
        let change = Self.savedPillChange(from: observedSave, to: save)
        observedSave = ObservedSave(save: save)
        switch change {
        case .show: showSavedPill()
        case .hide: hideSavedPill()
        case .keep: break
        }
    }

    private func hideSavedPill() {
        savedPillGeneration += 1
        guard savedPillIsVisible else { return }
        withAnimation { savedPillIsVisible = false }
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
