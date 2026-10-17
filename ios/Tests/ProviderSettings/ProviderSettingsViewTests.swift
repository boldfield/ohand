import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real provider settings screen in a window over the real model. The screen reports where each visible part
/// landed, the words it shows and whether it is enabled; the tests drive the model calls the screen's controls make and
/// then read what is on screen.
@MainActor
final class ProviderSettingsViewTests: XCTestCase {
    private static let phone = CGSize(width: 390, height: 844)
    private static let minimumTouchTarget: CGFloat = 44

    private var scope: CompositionReadScope = .authenticated
    private var rendered: [String: ProviderSettingsRenderedElement] = [:]
    private var window: UIWindow?
    private var buffers: [String: ProviderSecretBuffer] = [:]
    private let journal = FakeProviderSettingsJournal()

    private static let mixedListing = providerListing(
        supportLevels: [
            ("hosted", "hosted-v1", "open_ai", false, "supported"),
            ("private", "private-v1", "self_hosted", false, "unverified"),
        ], defaultVersion: nil)

    /// Shows the screen over `controls`, lets its first read land, runs `body`, then takes the screen away.
    private func withScreen(
        _ controls: FakeProviderSettingsControls,
        credentials: FakeProviderSettingsCredentials = FakeProviderSettingsCredentials(),
        _ body: (ProviderSettingsModel) async throws -> Void
    ) async rethrows {
        rendered = [:]
        buffers = [:]
        let model = ProviderSettingsModel(
            controls: controls, credentials: credentials, journal: journal, readScope: { [unowned self] in scope })
        let hosting = UIHostingController(
            rootView: NavigationStack {
                ProviderSettingsView(
                    model: model, renderReporter: { [unowned self] in rendered = $0 },
                    secretBufferProbe: { [unowned self] in buffers[$0] = $1 })
            })
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = hosting
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        do {
            try await body(model)
        } catch {
            close()
            throw error
        }
        close()
    }

    private func close() {
        window?.isHidden = true
        window = nil
    }

    private func settle() async {
        for _ in 0..<3 {
            window?.rootViewController?.view.setNeedsLayout()
            window?.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        window?.rootViewController?.view.layoutIfNeeded()
    }

    private func text(_ identifier: String) -> String? { rendered[identifier]?.text }

    private func assertTappable(
        _ identifier: String, enabled: Bool = true, file: StaticString = #filePath, line: UInt = #line
    ) {
        guard let element = rendered[identifier] else {
            return XCTFail("\(identifier) is shown", file: file, line: line)
        }
        XCTAssertGreaterThanOrEqual(element.frame.height, Self.minimumTouchTarget, identifier, file: file, line: line)
        XCTAssertEqual(element.isEnabled, enabled, "\(identifier) enabled", file: file, line: line)
    }

    func testLocalOnlyModeAndStoredVersionsWithTheirSupportStateAreShown() async {
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { _ in
            XCTAssertEqual(text(ProviderSettingsView.modeIdentifier), ProviderSettingsView.modeText(.localOnly))
            XCTAssertEqual(text(ProviderSettingsView.defaultNoteIdentifier), ProviderSettingsText.defaultChangeNote)
            XCTAssertNil(rendered[ProviderSettingsView.localOnlyIdentifier], "no default to clear")

            XCTAssertEqual(
                text(ProviderSettingsView.capabilityIdentifier("hosted-v1", "text_interpretation")),
                ProviderSettingsText.capabilityStatus("text_interpretation", support: "supported"))
            let unverified = text(ProviderSettingsView.capabilityIdentifier("private-v1", "text_interpretation")) ?? ""
            XCTAssertTrue(unverified.contains("unavailable, not verified"), unverified)
            XCTAssertTrue(unverified.contains("will not process notes"), unverified)

            assertTappable(ProviderSettingsView.makeDefaultIdentifier("hosted-v1"))
            XCTAssertNil(
                rendered[ProviderSettingsView.makeDefaultIdentifier("private-v1")],
                "an unverified private server is not offered as a default")
        }
    }

    func testTheFormInputSaveAndDefaultFlowShowsEachStepOnScreen() async {
        let controls = FakeProviderSettingsControls(listing: Self.mixedListing)
        await withScreen(controls) { model in
            assertTappable(ProviderSettingsView.saveIdentifier, enabled: false)
            XCTAssertNotNil(rendered[ProviderSettingsView.issuesIdentifier], "an empty form lists what is missing")

            model.draft = ProviderSettingsDraft(
                profileName: "work", providerProtocol: .openAI, endpoint: "", model: "synthetic-model",
                credentialReference: "cred-1", timeoutSeconds: 60)
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.nameFieldIdentifier), "work")
            XCTAssertEqual(text(ProviderSettingsView.modelFieldIdentifier), "synthetic-model")
            XCTAssertEqual(text(ProviderSettingsView.credentialFieldIdentifier), "cred-1")
            XCTAssertNil(rendered[ProviderSettingsView.issuesIdentifier])
            assertTappable(ProviderSettingsView.saveIdentifier)

            await model.save()
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.saveStateIdentifier), ProviderSettingsActionState.saved.message)
            XCTAssertEqual(controls.created.count, 1)

            await model.makeDefault(model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!)
            await settle()
            XCTAssertEqual(
                text(ProviderSettingsView.defaultStateIdentifier), ProviderSettingsActionState.defaultChanged.message)
            XCTAssertEqual(controls.defaultsSet, ["hosted-v1"])
        }
    }

    func testAStoredUnverifiedDefaultIsShownAsUnableToProcess() async {
        let listing = providerListing(
            supportLevels: [("private", "private-v1", "self_hosted", false, "unverified")], defaultVersion: "private-v1")
        await withScreen(FakeProviderSettingsControls(listing: listing)) { _ in
            XCTAssertEqual(
                text(ProviderSettingsView.defaultUnavailableIdentifier), ProviderSettingsText.unavailableDefaultNotice)
            assertTappable(ProviderSettingsView.localOnlyIdentifier)
        }
    }

    func testARefusedSaveShowsTheFailureAndNeverSaysSaved() async {
        let controls = FakeProviderSettingsControls(listing: Self.mixedListing)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "endpoint_invalid", message: "synthetic")))
        await withScreen(controls) { model in
            model.draft = ProviderSettingsDraft(
                profileName: "work", providerProtocol: .openAI, endpoint: "", model: "m", credentialReference: "c")
            await model.save()
            await settle()
            let shown = text(ProviderSettingsView.saveStateIdentifier) ?? ""
            XCTAssertEqual(shown, ProviderSettingsText.failure(controls.createResult.failureValueForView!))
            XCTAssertNotEqual(shown, ProviderSettingsActionState.saved.message)
        }
    }

    func testSwitchingFromAPrivateServerToAHostedProviderDropsTheHiddenAddress() async {
        let controls = FakeProviderSettingsControls(listing: Self.mixedListing)
        await withScreen(controls) { model in
            model.draft = ProviderSettingsDraft(
                profileName: "work", providerProtocol: .selfHosted, endpoint: "https://server.example/v1",
                model: "synthetic-model", credentialReference: "cred-1", timeoutSeconds: 60)
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.endpointFieldIdentifier), "https://server.example/v1")
            assertTappable(ProviderSettingsView.saveIdentifier)

            // What the provider type picker does through its binding.
            model.draft.providerProtocol = .openAI
            await settle()
            XCTAssertNil(rendered[ProviderSettingsView.endpointFieldIdentifier], "a hosted provider takes no address")
            XCTAssertEqual(model.draft.endpoint, "")
            XCTAssertNil(rendered[ProviderSettingsView.issuesIdentifier], "no hidden value blocks saving")
            assertTappable(ProviderSettingsView.saveIdentifier)

            await model.save()
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.saveStateIdentifier), ProviderSettingsActionState.saved.message)
            XCTAssertEqual(controls.created.count, 1)
            XCTAssertNil(controls.created.first?.endpoint)
            XCTAssertEqual(controls.created.first?.providerProtocol, "open_ai")

            model.draft.providerProtocol = .selfHosted
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.endpointFieldIdentifier), "", "the address field returns empty")
        }
    }

    func testEverySelfHostedCapabilityIsNamedAndUnavailableOnesGiveTheirReason() async {
        let capabilities = [
            ("text_interpretation", "unverified"), ("transcription", "unsupported"), ("embeddings", "unsupported"),
            ("speech_generation", "unsupported"),
        ]
        let capabilityJSON = capabilities.map { #"{"capability":"\#($0.0)","support":"\#($0.1)"}"# }
            .joined(separator: ",")
        let json = #"{"operation_id":1,"profiles":[{"profile_id":"private","versions":[{"profile_version":"private-v1","#
            + #""protocol":"self_hosted","endpoint":"https://server.example/v1","model":"synthetic-model","#
            + #""capabilities":[\#(capabilityJSON)],"credential_bound":true,"revoked":false,"default_for":[],"#
            + #""created_at":"2026-10-01T00:00:00Z"}]}],"defaults":[]}"#
        let listing = try! JSONDecoder().decode(ProviderProfileListAnswer.self, from: Data(json.utf8))

        await withScreen(FakeProviderSettingsControls(listing: listing)) { _ in
            let expected = [
                "text_interpretation":
                    "Text interpretation: unavailable, not verified. It will not process notes until verification "
                    + "evidence exists.",
                "transcription":
                    "Transcription: unavailable. Notes are transcribed on this device only; no provider transcribes them.",
                "embeddings": "Embeddings: unavailable. This version of the app does not use embeddings.",
                "speech_generation":
                    "Speech generation: unavailable. This version of the app does not use speech generation.",
            ]
            for (capability, status) in expected {
                XCTAssertEqual(
                    text(ProviderSettingsView.capabilityIdentifier("private-v1", capability)), status, capability)
            }
            let shown = expected.keys.compactMap { text(ProviderSettingsView.capabilityIdentifier("private-v1", $0)) }
            XCTAssertEqual(Set(shown).count, 4, "each capability row is distinguishable")
            XCTAssertFalse(shown.contains { $0.contains("Unsupported capability") })
            XCTAssertNil(rendered[ProviderSettingsView.makeDefaultIdentifier("private-v1")])
        }
    }

    // MARK: the hosted secure field

    private func secureFields(in view: UIView?) -> [UITextField] {
        guard let view else { return [] }
        let own = (view as? UITextField).flatMap { $0.isSecureTextEntry ? [$0] : [] } ?? []
        return own + view.subviews.flatMap { secureFields(in: $0) }
    }

    private func secureField(_ identifier: String, file: StaticString = #filePath, line: UInt = #line) -> UITextField? {
        let fields = secureFields(in: window?.rootViewController?.view)
        let field = fields.first { $0.accessibilityIdentifier == identifier }
        if field == nil { XCTFail("secure field \(identifier) is hosted", file: file, line: line) }
        return field
    }

    /// Types into the real secure field the way the keyboard does, then presses its return key.
    private func typeAndSubmit(_ secret: String, into identifier: String) async {
        guard let field = secureField(identifier) else { return }
        field.text = secret
        field.sendActions(for: .editingChanged)
        await settle()
        _ = field.delegate?.textFieldShouldReturn?(field)
        await settle()
    }

    func testTheHostedSecretFieldIsEmptyAfterStoreSucceedsAndAfterItFails() async {
        let credentials = FakeProviderSettingsCredentials()
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { _ in
            await typeAndSubmit(secret, into: ProviderSettingsView.secretFieldIdentifier)
            XCTAssertEqual(credentials.added, [Data(secret.utf8)], "the secret went to the credential service")
            XCTAssertEqual(secureField(ProviderSettingsView.secretFieldIdentifier)?.text ?? "", "")
            XCTAssertEqual(buffers[ProviderSettingsView.secretFieldIdentifier]?.text, "")
            XCTAssertEqual(
                text(ProviderSettingsView.credentialStateIdentifier), ProviderSettingsCredentialState.stored.message)

            credentials.updateResult = .failure(CredentialError.invalidated)
            await typeAndSubmit(secret, into: ProviderSettingsView.secretFieldIdentifier)
            XCTAssertEqual(credentials.updated.count, 1)
            XCTAssertEqual(secureField(ProviderSettingsView.secretFieldIdentifier)?.text ?? "", "", "empty after a failure")
            XCTAssertEqual(buffers[ProviderSettingsView.secretFieldIdentifier]?.text, "")
            XCTAssertEqual(
                text(ProviderSettingsView.credentialStateIdentifier),
                ProviderSettingsText.credentialFailure(CredentialError.invalidated))
            XCTAssertFalse(rendered.values.contains { $0.text.contains(secret) })
        }
    }

    func testTheHostedSecretFieldNeverExposesTheSecretThroughAccessibilityOrTheRenderReport() async {
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { _ in
            guard let field = secureField(ProviderSettingsView.secretFieldIdentifier) else { return }
            field.text = secret
            field.sendActions(for: .editingChanged)
            await settle()
            XCTAssertEqual(buffers[ProviderSettingsView.secretFieldIdentifier]?.text, secret, "the field is live")
            XCTAssertFalse((field.accessibilityValue ?? "").contains(secret), "accessibility value")
            XCTAssertFalse((field.accessibilityLabel ?? "").contains(secret), "accessibility label")
            XCTAssertFalse(rendered.values.contains { $0.text.contains(secret) }, "render report")
        }
    }

    func testTheHostedSecretFieldsEmptyWhenTheAppResignsActiveAndWhenTheyLeaveTheScreen() async {
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { model in
            let fieldIdentifier = ProviderSettingsView.secretFieldIdentifier
            secureField(fieldIdentifier).map { $0.text = secret; $0.sendActions(for: .editingChanged) }
            await settle()
            XCTAssertEqual(buffers[fieldIdentifier]?.text, secret)
            NotificationCenter.default.post(name: UIApplication.willResignActiveNotification, object: nil)
            await settle()
            XCTAssertEqual(buffers[fieldIdentifier]?.text, "", "emptied when the app leaves the foreground")
            XCTAssertEqual(secureField(fieldIdentifier)?.text ?? "", "")

            secureField(fieldIdentifier).map { $0.text = secret; $0.sendActions(for: .editingChanged) }
            await settle()
            XCTAssertEqual(buffers[fieldIdentifier]?.text, secret)
            // A relock takes the form, and with it the field, off the screen.
            scope = .captureOnly
            await model.refresh()
            await settle()
            XCTAssertNil(rendered[ProviderSettingsView.storeSecretIdentifier])
            XCTAssertEqual(buffers[fieldIdentifier]?.text, "", "emptied when the field leaves the screen")
        }
    }

    func testTheReplaceSecretEntryIsDisabledWhileItsOperationRuns() async throws {
        let credentials = FakeProviderSettingsCredentials()
        var release: CheckedContinuation<Void, Never>?
        credentials.suspendReplaceByVersion = { await withCheckedContinuation { release = $0 } }
        try await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { model in
            assertTappable(ProviderSettingsView.replaceSavedSecretIdentifier("hosted-v1"))
            let hosted = try XCTUnwrap(
                model.listing?.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" })
            let running = Task { await model.replaceSavedCredential(Data("synthetic-secret-value".utf8), for: hosted) }
            await settle()
            assertTappable(ProviderSettingsView.replaceSavedSecretIdentifier("hosted-v1"), enabled: false)
            XCTAssertTrue(secureField(ProviderSettingsView.savedSecretFieldIdentifier("hosted-v1"))?.isEnabled == false)

            release?.resume()
            await running.value
            await settle()
            assertTappable(ProviderSettingsView.replaceSavedSecretIdentifier("hosted-v1"))
        }
    }

    func testEverySubmissionEmptiesTheSecretBufferWhetherOrNotItIsUsable() {
        let buffer = ProviderSecretBuffer()

        buffer.text = "synthetic-secret-value"
        XCTAssertEqual(buffer.take(), .secret(Data("synthetic-secret-value".utf8)))
        XCTAssertEqual(buffer.text, "", "emptied after a usable secret")

        buffer.text = ""
        XCTAssertEqual(buffer.take(), .rejected(.empty))
        XCTAssertEqual(buffer.text, "")

        buffer.text = " synthetic-secret-value\n"
        XCTAssertEqual(
            buffer.take(), .rejected(.surroundingWhitespace),
            "input that would need trimming is refused, not changed")
        XCTAssertEqual(buffer.text, "", "emptied after a refused secret")

        buffer.text = "synthetic-secret-value"
        buffer.clear()
        XCTAssertEqual(buffer.text, "", "emptied when the screen goes away or the app leaves the foreground")
    }

    func testStoredVersionsOfferReplaceCheckAndRemoveAndEmptyVersionsOfferNone() async {
        let listing = providerListing(
            supportLevels: [
                ("hosted", "hosted-v1", "open_ai", false, "supported"),
                ("old", "old-v1", "anthropic", true, "supported"),
            ], defaultVersion: nil)
        await withScreen(FakeProviderSettingsControls(listing: listing)) { _ in
            assertTappable(ProviderSettingsView.replaceSavedSecretIdentifier("hosted-v1"))
            assertTappable(ProviderSettingsView.checkSavedSecretIdentifier("hosted-v1"))
            assertTappable(ProviderSettingsView.removeSavedSecretIdentifier("hosted-v1"))
            assertTappable(ProviderSettingsView.checkConnectionIdentifier("hosted-v1"))
            XCTAssertNil(rendered[ProviderSettingsView.replaceSavedSecretIdentifier("old-v1")], "a revoked version is closed")
            XCTAssertNil(rendered[ProviderSettingsView.removeSavedSecretIdentifier("old-v1")])
        }
    }

    func testTheSecretIsNeverRenderedAndFeedbackIsFixedTextAtTheDraftAndTheStoredVersion() async throws {
        let credentials = FakeProviderSettingsCredentials()
        let secret = "synthetic-secret-value"
        try await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { model in
            assertTappable(ProviderSettingsView.storeSecretIdentifier)
            assertTappable(ProviderSettingsView.checkSecretIdentifier)
            assertTappable(ProviderSettingsView.removeSecretIdentifier)

            model.storeCredential(Data(secret.utf8))
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.credentialStateIdentifier), ProviderSettingsCredentialState.stored.message)
            XCTAssertEqual(text(ProviderSettingsView.credentialFieldIdentifier), "11111111-1111-1111-1111-111111111111")

            let hosted = try XCTUnwrap(model.listing?.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" })
            await model.replaceSavedCredential(Data(secret.utf8), for: hosted)
            await settle()
            XCTAssertEqual(
                text(ProviderSettingsView.savedSecretStateIdentifier("hosted-v1")),
                ProviderSettingsCredentialState.updated.message)

            credentials.replaceByVersionResult = .failure(CredentialError.invalidated)
            await model.replaceSavedCredential(Data(secret.utf8), for: hosted)
            await settle()
            XCTAssertEqual(
                text(ProviderSettingsView.savedSecretStateIdentifier("hosted-v1")),
                ProviderSettingsText.credentialFailure(CredentialError.invalidated))

            XCTAssertFalse(rendered.values.contains { $0.text.contains(secret) }, "no rendered element shows the secret")
            XCTAssertEqual(credentials.replacedByVersion.map(\.1), ["hosted-v1", "hosted-v1"])
        }
    }

    func testLeavingTheScreenDeletesAnUnsavedSecretAndRemovesTheSavedControls() async {
        let credentials = FakeProviderSettingsCredentials()
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { model in
            model.storeCredential(Data("synthetic-secret-value".utf8))
            model.clear()
            await settle()
            XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
            XCTAssertEqual(model.draft.credentialReference, "")
        }
    }

    func testALockedSessionShowsOnlyTheLockedNotice() async {
        scope = .captureOnly
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { _ in
            XCTAssertEqual(text(ProviderSettingsView.lockedIdentifier), ProviderSettingsText.lockedText)
            XCTAssertNil(rendered[ProviderSettingsView.modeIdentifier])
            XCTAssertNil(rendered[ProviderSettingsView.saveIdentifier])
            XCTAssertNil(rendered[ProviderSettingsView.makeDefaultIdentifier("hosted-v1")])
        }
    }
}

private extension Result {
    var failureValueForView: Failure? {
        if case .failure(let failure) = self { return failure }
        return nil
    }
}
