import SwiftUI
import OhAndCoreBridge

/// The suggestion screen: one item and three plain answers, or a calm line when nothing is eligible. Everything it
/// shows comes from `SuggestionScreenModel`.
struct SuggestionView: View {
    static let textIdentifier = "suggestion.text"
    static let notNowIdentifier = "suggestion.not-now"
    static let stopIdentifier = "suggestion.stop"
    static let doneIdentifier = "suggestion.done"
    static let messageIdentifier = "suggestion.message"
    static let noticeIdentifier = "suggestion.notice"

    static let noTextLine = "This item has no text to show yet."
    static let nothingToOfferLine = "Nothing to look at right now."
    static let lockedLine = "Unlock Oh And to see your suggestion."
    static let unavailableLine = "Your suggestion could not be loaded just now."
    static let loadingLine = "Looking…"

    static func text(for response: SuggestionResponseKind) -> String {
        switch response {
        case .notNow: return "Not now"
        case .stopSuggesting: return "Stop suggesting this"
        case .done: return "Done"
        }
    }

    static func text(forResponded response: SuggestionResponseKind) -> String {
        switch response {
        case .notNow: return "Okay, not now."
        case .stopSuggesting: return "Okay, this will not be suggested again."
        case .done: return "Marked done."
        }
    }

    static func text(for notice: SuggestionScreenNotice) -> String {
        switch notice {
        case .changed: return "That item changed, so this is what is current."
        case .notSaved: return "That answer could not be saved. Nothing changed; you can try again."
        }
    }

    static func identifier(for response: SuggestionResponseKind) -> String {
        switch response {
        case .notNow: return notNowIdentifier
        case .stopSuggesting: return stopIdentifier
        case .done: return doneIdentifier
        }
    }

    /// Identifier to the text shown for it, collected so a hosted test can see what is rendered.
    struct RenderedKey: PreferenceKey {
        static var defaultValue: [String: String] = [:]

        static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
            value.merge(nextValue()) { _, latest in latest }
        }
    }

    @ObservedObject var model: SuggestionScreenModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            switch model.state {
            case .offer(let offer):
                Text(offer.text ?? Self.noTextLine)
                    .font(.title3)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.textIdentifier)
                    .preference(key: RenderedKey.self, value: [Self.textIdentifier: offer.text ?? Self.noTextLine])
                ForEach(offer.responses, id: \.self) { response in
                    Button(Self.text(for: response)) { Task { await model.respond(response) } }
                        .disabled(model.isBusy)
                        .accessibilityIdentifier(Self.identifier(for: response))
                        .preference(key: RenderedKey.self, value: [Self.identifier(for: response): Self.text(for: response)])
                }
            case .idle, .loading:
                message(Self.loadingLine)
            case .nothingToOffer:
                message(Self.nothingToOfferLine)
            case .responded(let response):
                message(Self.text(forResponded: response))
            case .locked:
                message(Self.lockedLine)
            case .unavailable:
                message(Self.unavailableLine)
            }
            if let notice = model.notice {
                Text(Self.text(for: notice))
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.noticeIdentifier)
                    .preference(key: RenderedKey.self, value: [Self.noticeIdentifier: Self.text(for: notice)])
            }
            Spacer()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding()
        .navigationTitle("Suggestion")
        .navigationBarTitleDisplayMode(.inline)
        .onPreferenceChange(RenderedKey.self) { model.renderReporter?($0) }
        .task(id: model.promptID) { await model.open(notificationID: model.promptID) }
        .onDisappear { model.lock() }
    }

    private func message(_ line: String) -> some View {
        Text(line)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(Self.messageIdentifier)
            .preference(key: RenderedKey.self, value: [Self.messageIdentifier: line])
    }
}
