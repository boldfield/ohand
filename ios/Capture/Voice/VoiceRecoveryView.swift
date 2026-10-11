import SwiftUI
import UIKit

private struct VoiceRecoveryLayoutKey: PreferenceKey {
    static var defaultValue: [String: CGRect] = [:]

    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

private extension View {
    func reportFrame(_ identifier: String) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(key: VoiceRecoveryLayoutKey.self, value: [identifier: proxy.frame(in: .global)])
        })
    }
}

extension VoiceRecoveryAction {
    var label: String {
        switch self {
        case .finish: return "Finish and save"
        case .continueRecording: return "Add audio"
        case .delete: return "Delete"
        }
    }

    fileprivate var identifierSuffix: String {
        switch self {
        case .finish: return "finish"
        case .continueRecording: return "continue"
        case .delete: return "delete"
        }
    }
}

/// What re-entry found, with the choices the user has for each unfinished voice recording: finish it (saved only once
/// the core confirms), add audio to it, or delete it after confirming. Checking runs while the view is shown and never
/// stands between the user and normal capture.
///
/// Every frame the view reports is keyed by the same identifiers it gives the accessibility tree, so layout can be
/// tested without a device.
struct VoiceRecoveryView: View {
    static let statusIdentifier = "voice-recovery.status"
    static let emptyIdentifier = "voice-recovery.empty"
    static let checkingIdentifier = "voice-recovery.checking"
    static let recordingIdentifier = "voice-recovery.recording"
    static let stopIdentifier = "voice-recovery.recording.stop"
    static let cancelIdentifier = "voice-recovery.recording.cancel"

    static func rowIdentifier(_ fileName: String) -> String { "voice-recovery.row.\(fileName)" }
    static func noticeIdentifier(_ index: Int) -> String { "voice-recovery.notice.\(index)" }

    static func actionIdentifier(_ action: VoiceRecoveryAction, fileName: String) -> String {
        "voice-recovery.row.\(fileName).\(action.identifierSuffix)"
    }

    @ObservedObject var model: VoiceRecoveryModel
    var layoutReporter: (([String: CGRect]) -> Void)?

    @Environment(\.sizeCategory) private var sizeCategory

    init(model: VoiceRecoveryModel, layoutReporter: (([String: CGRect]) -> Void)? = nil) {
        self.model = model
        self.layoutReporter = layoutReporter
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let status = model.status {
                    Text(status)
                        .font(.callout)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.statusIdentifier)
                        .reportFrame(Self.statusIdentifier)
                }
                if model.isRecording {
                    recordingControls
                }
                ForEach(Array(model.notices.enumerated()), id: \.offset) { index, notice in
                    Text(notice)
                        .font(.callout)
                        .foregroundColor(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.noticeIdentifier(index))
                        .reportFrame(Self.noticeIdentifier(index))
                }
                ForEach(model.rows, id: \.fileName) { row in
                    recordingRow(row)
                }
                if model.rows.isEmpty && model.notices.isEmpty {
                    Text(model.isChecking ? "Checking for unfinished recordings\u{2026}" : "No unfinished recordings.")
                        .font(.callout)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier(model.isChecking ? Self.checkingIdentifier : Self.emptyIdentifier)
                        .reportFrame(model.isChecking ? Self.checkingIdentifier : Self.emptyIdentifier)
                }
            }
            .padding()
        }
        .onAppear { model.refresh() }
        .onReceive(model.$status) { status in
            if let status = status {
                UIAccessibility.post(notification: .announcement, argument: status)
            }
        }
        .onPreferenceChange(VoiceRecoveryLayoutKey.self) { frames in
            layoutReporter?(frames)
        }
        .confirmationDialog(
            "Delete this recording?",
            isPresented: Binding(
                get: { model.deletionRequestFileName != nil },
                set: { isPresented in if !isPresented { model.cancelDeletion() } }),
            titleVisibility: .visible,
            presenting: model.deletionRequestFileName
        ) { fileName in
            Button("Delete", role: .destructive) { model.confirmDeletion(fileName: fileName) }
            Button("Keep it", role: .cancel) { model.cancelDeletion() }
        } message: { _ in
            Text("It is not saved anywhere else. Deleting it cannot be undone.")
        }
    }

    private var recordingControls: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Recording audio now.")
                .font(.headline)
                .accessibilityIdentifier(Self.recordingIdentifier)
            actionButtons {
                Button(action: { model.stopRecording() }) {
                    Text("Stop and add").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Self.stopIdentifier)
                .reportFrame(Self.stopIdentifier)
                Button(action: { model.cancelRecording() }) {
                    Text("Cancel").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Self.cancelIdentifier)
                .reportFrame(Self.cancelIdentifier)
            }
        }
    }

    private func recordingRow(_ row: VoiceRecoveryRow) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(row.title)
                .font(.headline)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            Text(row.detail)
                .font(.callout)
                .foregroundColor(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            actionButtons {
                ForEach(row.actions, id: \.self) { action in
                    Button(role: action == .delete ? .destructive : nil, action: {
                        model.perform(action, onFileName: row.fileName)
                    }) {
                        Text(action.label).frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .buttonStyle(.bordered)
                    .disabled(!model.isEnabled(action))
                    .accessibilityIdentifier(Self.actionIdentifier(action, fileName: row.fileName))
                    .reportFrame(Self.actionIdentifier(action, fileName: row.fileName))
                }
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 12).fill(Color(.secondarySystemBackground)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.rowIdentifier(row.fileName))
        .reportFrame(Self.rowIdentifier(row.fileName))
    }

    @ViewBuilder
    private func actionButtons<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        if sizeCategory.isAccessibilityCategory {
            VStack(spacing: 8) { content() }
        } else {
            HStack(spacing: 8) { content() }
        }
    }
}
