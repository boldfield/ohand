import SwiftUI
import UIKit

/// What the status screen shows, keyed by accessibility identifier, so a hosted test can read it.
struct StatusRenderedKey: PreferenceKey {
    static var defaultValue: [String: String] = [:]

    static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

/// The actions the screen's buttons run, keyed by accessibility identifier, so a hosted test can trigger them. Two
/// values are equal when they hold the same buttons: each action only calls the one model the screen was built over.
struct StatusRenderedActions: Equatable {
    var byIdentifier: [String: () -> Void] = [:]

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.byIdentifier.keys == rhs.byIdentifier.keys }
}

struct StatusActionsKey: PreferenceKey {
    static var defaultValue = StatusRenderedActions()

    static func reduce(value: inout StatusRenderedActions, nextValue: () -> StatusRenderedActions) {
        value.byIdentifier.merge(nextValue().byIdentifier) { _, latest in latest }
    }
}

/// The status screen. Everything it shows comes from `StatusModel`; it is reachable but never required.
struct StatusView: View {
    static let retryIdentifier = "status.retry"
    static let openSettingsIdentifier = "status.open-settings"
    static let refreshIdentifier = "status.refresh"
    static let providerSettingsIdentifier = "status.provider-settings"
    static let retryNoteIdentifier = "status.retry-note"
    static let retryNotesIdentifier = "status.retry-notes"
    static let rerunSummaryIdentifier = "status.rerun-summary"
    static let processingSettingsIdentifier = "status.processing-settings"
    static let unlockIdentifier = "status.unlock"
    static let showAllIdentifier = "status.show-all"
    static func noteIdentifier(_ itemID: String) -> String { "status.failed-note.\(itemID)" }
    static func rowIdentifier(_ kind: StatusFactKind) -> String { "status.row.\(kind.rawValue)" }

    static let infoParagraphs = [
        StatusText.introText,
        "Trying again only asks the waiting work to run once more from the saved original. It never changes what you wrote.",
    ]

    @ObservedObject var model: StatusModel
    /// The Provider settings screen this one links to when a destination needs the person.
    var makeProviderSettings: () -> AnyView = { AnyView(EmptyView()) }
    /// Lets a hosted test see what the screen shows; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?
    /// Lets a hosted test run what a tap on each button runs; production leaves it unset.
    var actionReporter: (([String: () -> Void]) -> Void)?

    /// The notes whose sorting failed, directly under the processing fact that says some work needs the person. Locked,
    /// only the fact's own count-only wording shows, with a way to unlock.
    @ViewBuilder private func failedNotesRows(for fact: StatusFact) -> some View {
        if model.failedNotesAccess == .locked, fact.tone == .needsAttention, let unlock = model.requestUnlock {
            Button(StatusText.unlockLabel, action: unlock)
                .font(DSFont.rowLabel)
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .accessibilityIdentifier(Self.unlockIdentifier)
                .preference(key: StatusRenderedKey.self, value: [Self.unlockIdentifier: StatusText.unlockLabel])
        }
        ForEach(model.listedNotes) { note in
            FailedNoteRow(note: note, model: model)
        }
        if model.hasMoreNotes {
            NavigationLink {
                FailedNotesListView(model: model, makeProviderSettings: makeProviderSettings)
            } label: {
                DSRow(label: StatusText.showAllLabel, value: "\(model.failedNotes.count)")
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier(Self.showAllIdentifier)
            .preference(key: StatusRenderedKey.self, value: [Self.showAllIdentifier: StatusText.showAllLabel])
        }
        FailedNotesActions(model: model, makeProviderSettings: makeProviderSettings)
    }

    private var retryAction: () -> Void { { Task { await model.retry() } } }

    var body: some View {
        SettingsScreenChrome(title: "Status", infoTitle: "Status", infoParagraphs: Self.infoParagraphs) {
            DSGroupedSection(footer: StatusText.introText) {
                ForEach(model.facts) { fact in
                    VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                        Text(fact.headline)
                            .font(DSFont.rowLabel)
                            .foregroundColor(DSColor.ink.color)
                        Text(fact.detail)
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.horizontal, DSSpacing.md)
                    .padding(.vertical, DSSpacing.xs)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier(Self.rowIdentifier(fact.kind))
                    .preference(key: StatusRenderedKey.self, value: [Self.rowIdentifier(fact.kind): "\(fact.headline) | \(fact.detail)"])
                    if fact.kind == .processing { failedNotesRows(for: fact) }
                }
            }
            if model.facts.contains(where: { $0.actions.contains(.retry) }) {
                Button(StatusText.retryLabel, action: retryAction)
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.retryIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.retryIdentifier: StatusText.retryLabel])
                    .preference(key: StatusActionsKey.self, value: StatusRenderedActions(byIdentifier: [Self.retryIdentifier: retryAction]))
            }
            if model.retryRequested {
                Text(StatusText.retryRequestedNote)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, DSSpacing.md)
                    .accessibilityIdentifier(Self.retryNoteIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.retryNoteIdentifier: StatusText.retryRequestedNote])
            }
            if model.facts.contains(where: { $0.actions.contains(.openProviderSettings) }) {
                NavigationLink(StatusText.providerSettingsLabel) { makeProviderSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.providerSettingsIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.providerSettingsIdentifier: StatusText.providerSettingsLabel])
            }
            if model.facts.contains(where: { $0.actions.contains(.openSettings) }) {
                Button(StatusText.openSettingsLabel) { model.openSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.openSettingsIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.openSettingsIdentifier: StatusText.openSettingsLabel])
            }
            Button(StatusText.refreshLabel) { Task { await model.refresh() } }
                .font(DSFont.rowLabel)
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .accessibilityIdentifier(Self.refreshIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.refreshIdentifier: StatusText.refreshLabel])
        }
        .onPreferenceChange(StatusRenderedKey.self) { renderReporter?($0) }
        .onPreferenceChange(StatusActionsKey.self) { actionReporter?($0.byIdentifier) }
        .task { await model.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            Task { await model.refresh() }
        }
    }
}

/// One failed note: the first line of its text, when it was saved and one plain reason.
struct FailedNoteRow: View {
    let note: FailedNote
    @ObservedObject var model: StatusModel

    private var firstLine: String { model.firstLines[note.itemID] ?? StatusText.notePlaceholder }
    private var captured: String { StatusText.capturedLabel(note.capturedAt) }

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xxs) {
            Text(firstLine)
                .font(DSFont.rowLabel)
                .foregroundColor(DSColor.ink.color)
                .lineLimit(2)
            Text(captured)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
            Text(note.reason)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, DSSpacing.md)
        .padding(.vertical, DSSpacing.xs)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(StatusView.noteIdentifier(note.itemID))
        .preference(
            key: StatusRenderedKey.self,
            value: [StatusView.noteIdentifier(note.itemID): "\(firstLine) | \(captured) | \(note.reason)"]
        )
        .task { await model.ensureFirstLine(itemID: note.itemID) }
    }
}

/// "Try again" for the listed failed notes, the one line saying what happened and, when a setting is the cause, the
/// way to Processing.
struct FailedNotesActions: View {
    @ObservedObject var model: StatusModel
    var makeProviderSettings: () -> AnyView

    private var retryAction: () -> Void { { Task { await model.retryFailedNotes() } } }

    var body: some View {
        if model.offersNoteRetry {
            Button(StatusText.retryLabel, action: retryAction)
                .font(DSFont.rowLabel)
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .disabled(model.isRerunning)
                .accessibilityIdentifier(StatusView.retryNotesIdentifier)
                .preference(key: StatusRenderedKey.self, value: [StatusView.retryNotesIdentifier: StatusText.retryLabel])
                .preference(
                    key: StatusActionsKey.self,
                    value: StatusRenderedActions(byIdentifier: [StatusView.retryNotesIdentifier: retryAction]))
        }
        if let summary = model.rerunSummary {
            Text(summary.text)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, DSSpacing.md)
                .accessibilityIdentifier(StatusView.rerunSummaryIdentifier)
                .preference(key: StatusRenderedKey.self, value: [StatusView.rerunSummaryIdentifier: summary.text])
            if summary.needsSetting {
                NavigationLink(StatusText.processingSettingsLabel) { makeProviderSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(StatusView.processingSettingsIdentifier)
                    .preference(
                        key: StatusRenderedKey.self,
                        value: [StatusView.processingSettingsIdentifier: StatusText.processingSettingsLabel])
            }
        }
    }
}

/// Every failed note, reached from "Show all".
struct FailedNotesListView: View {
    @ObservedObject var model: StatusModel
    var makeProviderSettings: () -> AnyView
    var renderReporter: (([String: String]) -> Void)?

    var body: some View {
        SettingsScreenChrome(
            title: StatusText.failedNotesTitle, infoTitle: StatusText.failedNotesTitle,
            infoParagraphs: [StatusView.infoParagraphs[1]]
        ) {
            DSGroupedSection {
                ForEach(model.failedNotes) { note in
                    FailedNoteRow(note: note, model: model)
                }
                FailedNotesActions(model: model, makeProviderSettings: makeProviderSettings)
            }
        }
        .onPreferenceChange(StatusRenderedKey.self) { renderReporter?($0) }
    }
}
