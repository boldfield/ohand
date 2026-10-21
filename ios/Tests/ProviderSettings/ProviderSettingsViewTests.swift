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
    private let sweep = FakeProviderSettingsSweep()
    private var navigation = ProviderSettingsNavigation()
    private var textSize = DynamicTypeSize.large
    private var appearance = UIUserInterfaceStyle.light
    private var offersOwnServer = true

    private static let mixedListing = providerListing(
        supportLevels: [
            ("hosted", "hosted-v1", "open_ai", false, "supported"),
            ("private", "private-v1", "self_hosted", false, "unverified"),
        ], defaultVersion: nil)

    /// The three screens of the provider settings. A hosted test cannot tap a link, so it shows the one it needs.
    private enum Screen {
        case main, add, advanced
    }

    /// Shows the screen over `controls`, lets its first read land, runs `body`, then takes the screen away.
    private func withScreen(
        _ controls: FakeProviderSettingsControls,
        showing screen: Screen = .advanced,
        credentials: FakeProviderSettingsCredentials = FakeProviderSettingsCredentials(),
        _ body: (ProviderSettingsModel) async throws -> Void
    ) async rethrows {
        rendered = [:]
        buffers = [:]
        navigation = ProviderSettingsNavigation()
        let model = ProviderSettingsModel(
            controls: controls, credentials: credentials, sweep: sweep, readScope: { [unowned self] in scope },
            offersOwnServer: offersOwnServer, captureRouteID: "r")
        let hosting = UIHostingController(
            rootView: NavigationStack {
                switch screen {
                case .main:
                    ProviderSettingsView(
                        model: model, renderReporter: { [unowned self] in rendered = $0 },
                        secretBufferProbe: { [unowned self] in buffers[$0] = $1 },
                        navigation: navigation)
                case .add:
                    ProviderAddView(
                        model: model, renderReporter: { [unowned self] in rendered = $0 },
                        secretBufferProbe: { [unowned self] in buffers[$0] = $1 })
                case .advanced:
                    ProviderAdvancedView(
                        model: model, renderReporter: { [unowned self] in rendered = $0 },
                        secretBufferProbe: { [unowned self] in buffers[$0] = $1 })
                }
            }
            .environment(\.dynamicTypeSize, textSize))
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = hosting
        shown.overrideUserInterfaceStyle = appearance
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
            XCTAssertEqual(text(ProviderSettingsView.modeIdentifier), ProviderSettingsView.modeSummary(.localOnly))
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
            assertTappable(ProviderSettingsView.saveIdentifier)
            XCTAssertNil(rendered[ProviderSettingsView.fieldIssueIdentifier(.model)], "an untouched form lists nothing")

            model.draft = ProviderSettingsDraft(
                profileName: "work", providerProtocol: .openAI, endpoint: "", model: "synthetic-model",
                credentialReference: "cred-1", timeoutSeconds: 60)
            await settle()
            XCTAssertEqual(text(ProviderSettingsView.nameFieldIdentifier), "work")
            XCTAssertEqual(text(ProviderSettingsView.modelFieldIdentifier), "synthetic-model")
            XCTAssertEqual(text(ProviderSettingsView.credentialFieldIdentifier), "cred-1")
            XCTAssertNil(rendered[ProviderSettingsView.fieldIssueIdentifier(.model)])
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

    // MARK: M1 F39, in both appearances

    private func inBothAppearances(_ run: (UIUserInterfaceStyle) async throws -> Void) async rethrows {
        for style in [UIUserInterfaceStyle.light, .dark] {
            appearance = style
            try await run(style)
        }
        appearance = .light
    }

    private static let anthropicListing = providerListing(
        supportLevels: [
            ("cloud", "cloud-v1", "anthropic", false, "supported"),
            ("private", "private-v1", "self_hosted", false, "supported"),
        ], defaultVersion: "cloud-v1")

    func testOwnServerIsNotOfferedButASavedOneIsListedAndCannotBeTheDefault() async {
        await inBothAppearances { style in
            offersOwnServer = false
            await withScreen(FakeProviderSettingsControls(listing: Self.anthropicListing), showing: .main) { _ in
                XCTAssertNil(rendered[ProviderSettingsView.choiceIdentifier(.ownServer)], "\(style)")
                XCTAssertNotNil(rendered[ProviderSettingsView.choiceIdentifier(.cloudProvider)], "\(style)")
            }
            await withScreen(FakeProviderSettingsControls(listing: Self.anthropicListing), showing: .advanced) { _ in
                XCTAssertNotNil(rendered[ProviderSettingsView.versionIdentifier("private-v1")] ?? rendered[ProviderSettingsView.checkConnectionIdentifier("private-v1")], "\(style)")
                XCTAssertEqual(
                    text(ProviderSettingsView.ownServerUnavailableIdentifier("private-v1")),
                    ProviderSettingsText.ownServerUnavailableNotice, "\(style)")
                XCTAssertNil(rendered[ProviderSettingsView.makeDefaultIdentifier("private-v1")], "\(style)")
            }
            offersOwnServer = true
        }
    }

    func testTheAllowanceLineShowsWhileACloudDefaultIsNotAllowedAndGoesOnceGranted() async {
        await inBothAppearances { style in
            let controls = FakeProviderSettingsControls(listing: Self.anthropicListing)
            await withScreen(controls, showing: .main) { model in
                XCTAssertEqual(
                    text(ProviderSettingsView.privacyAllowanceIdentifier), ProviderSettingsText.privacyAllowanceNotice, "\(style)")
                XCTAssertEqual(ProviderSettingsText.privacyAllowanceNotice, "Allow it under Privacy to start sending notes.")

                let granted = #"[{"route_id":"r","route_name":"Local capture","scope":"personal","granted_capabilities":[{"capability":"text_interpretation","destination_classes":["cloud"]}]}]"#
                controls.routePolicy = .success(try! JSONDecoder().decode([RoutePolicyRoute].self, from: Data(granted.utf8)))
                await model.refresh()
                await settle()
                XCTAssertNil(rendered[ProviderSettingsView.privacyAllowanceIdentifier], "\(style)")
            }
        }
    }

    func testTheAddFormShowsAProblemOnlyAfterSaveOrAnEditAndLabelsTheTypes() async {
        await inBothAppearances { style in
            await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: .add) { model in
                XCTAssertEqual(model.draft.providerProtocol, .anthropic, "\(style)")
                XCTAssertEqual(ProviderSettingsText.protocolName("open_ai"), "OpenAI")
                XCTAssertEqual(ProviderSettingsProtocol.offered(offersOwnServer: model.offersOwnServer).count, model.offersOwnServer ? 3 : 2)
                for field in [ProviderSettingsField.profileName, .model, .credential] {
                    XCTAssertNil(rendered[ProviderSettingsView.fieldIssueIdentifier(field)], "\(style) \(field)")
                }

                model.draft.model = " "
                await settle()
                XCTAssertEqual(text(ProviderSettingsView.fieldIssueIdentifier(.model)), ProviderSettingsText.issue(.modelRequired), "\(style)")
                XCTAssertNil(rendered[ProviderSettingsView.fieldIssueIdentifier(.profileName)], "\(style)")

                await model.save()
                await settle()
                XCTAssertEqual(
                    text(ProviderSettingsView.fieldIssueIdentifier(.profileName)), ProviderSettingsText.issue(.profileNameRequired), "\(style)")
                XCTAssertEqual(
                    text(ProviderSettingsView.fieldIssueIdentifier(.credential)), ProviderSettingsText.issue(.credentialRequired), "\(style)")
            }
        }
    }

    func testTheModelFieldGivesAGenericExampleAndTheInfoNoteSaysItMustMatch() {
        XCTAssertTrue(ProviderSettingsText.modelPlaceholder.contains("e.g."))
        XCTAssertTrue(ProviderSettingsText.modelNote.contains("match the provider's exactly"))
    }

    func testAStoredUnverifiedDefaultIsShownAsUnableToProcess() async {
        let listing = providerListing(
            supportLevels: [("private", "private-v1", "self_hosted", false, "unverified")], defaultVersion: "private-v1")
        await withScreen(FakeProviderSettingsControls(listing: listing)) { _ in
            XCTAssertEqual(
                text(ProviderSettingsView.defaultUnavailableIdentifier), ProviderSettingsText.unavailableDefaultNotice)
            assertTappable(ProviderSettingsView.localOnlyIdentifier)
        }
        await withScreen(FakeProviderSettingsControls(listing: listing), showing: .main) { _ in
            XCTAssertEqual(
                text(ProviderSettingsView.defaultUnavailableIdentifier), ProviderSettingsText.unavailableDefaultNotice)
        }
    }

    // MARK: the Processing screen

    func testTheProcessingScreenShowsTheThreeChoicesAndOffersOnlyWhatCanBeChosen() async {
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: .main) { model in
            for choice in ProviderProcessingChoice.allCases {
                assertTappable(ProviderSettingsView.choiceIdentifier(choice), enabled: choice != .ownServer)
                XCTAssertEqual(text(ProviderSettingsView.choiceIdentifier(choice)), choice.label)
            }
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.onThisPhone)]?.isSelected, true)
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.cloudProvider)]?.isSelected, false)
            XCTAssertEqual(
                model.candidateVersion(for: .cloudProvider)?.profileVersion, "hosted-v1")
            XCTAssertNil(model.candidateVersion(for: .ownServer), "an unverified private server is not offered")
            assertTappable(ProviderSettingsView.addProviderIdentifier)
            assertTappable(ProviderSettingsView.advancedIdentifier)
            XCTAssertNil(rendered[ProviderSettingsView.saveIdentifier], "the forms are one level down")
            XCTAssertNil(rendered[ProviderSettingsView.makeDefaultIdentifier("hosted-v1")])
        }
    }

    func testTheChoiceFollowsTheStoredDefault() async {
        let listing = providerListing(
            supportLevels: [
                ("hosted", "hosted-v1", "open_ai", false, "supported"),
                ("private", "private-v1", "self_hosted", false, "supported"),
            ], defaultVersion: "private-v1")
        let controls = FakeProviderSettingsControls(listing: listing)
        await withScreen(controls, showing: .main) { model in
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.ownServer)]?.isSelected, true)
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.onThisPhone)]?.isSelected, false)
            assertTappable(ProviderSettingsView.choiceIdentifier(.onThisPhone))
            assertTappable(ProviderSettingsView.choiceIdentifier(.cloudProvider))
            XCTAssertEqual(model.candidateVersion(for: .cloudProvider)?.profileVersion, "hosted-v1")

            // The fake does not move the default itself; the stored answer it gives next does.
            controls.listing = .success(providerListing(
                supportLevels: [
                    ("hosted", "hosted-v1", "open_ai", false, "supported"),
                    ("private", "private-v1", "self_hosted", false, "supported"),
                ], defaultVersion: "hosted-v1"))
            await model.makeDefault(model.candidateVersion(for: .cloudProvider)!)
            await settle()
            XCTAssertEqual(controls.defaultsSet, ["hosted-v1"])
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.cloudProvider)]?.isSelected, true)
            controls.listing = .success(providerListing(
                supportLevels: [
                    ("hosted", "hosted-v1", "open_ai", false, "supported"),
                    ("private", "private-v1", "self_hosted", false, "supported"),
                ], defaultVersion: nil))
            await model.useLocalOnly()
            await settle()
            XCTAssertEqual(controls.defaultsCleared, 1)
            XCTAssertEqual(rendered[ProviderSettingsView.choiceIdentifier(.onThisPhone)]?.isSelected, true)
        }
    }

    func testTheAdvancedScreenKeepsTheFormControlsAndTheAddScreenTheShortForm() async {
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: .advanced) { _ in
            // The timeout stepper is a system control that does not report a frame.
            for identifier in [
                ProviderSettingsView.nameFieldIdentifier, ProviderSettingsView.modelFieldIdentifier,
                ProviderSettingsView.credentialFieldIdentifier,
            ] {
                XCTAssertNotNil(rendered[identifier], identifier)
            }
            assertTappable(ProviderSettingsView.checkSecretIdentifier)
            assertTappable(ProviderSettingsView.removeSecretIdentifier)
            assertTappable(ProviderSettingsView.checkConnectionIdentifier("hosted-v1"))
        }
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: .add) { _ in
            XCTAssertNotNil(rendered[ProviderSettingsView.nameFieldIdentifier])
            XCTAssertNotNil(rendered[ProviderSettingsView.modelFieldIdentifier])
            assertTappable(ProviderSettingsView.storeSecretIdentifier)
            assertTappable(ProviderSettingsView.saveIdentifier)
            XCTAssertNil(rendered[ProviderSettingsView.checkSecretIdentifier])
            XCTAssertNil(rendered[ProviderSettingsView.checkConnectionIdentifier("hosted-v1")])
        }
    }

    func testARefusedSaveShowsTheFailureAndNeverSaysSaved() async {
        let controls = FakeProviderSettingsControls(listing: Self.mixedListing)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "endpoint_invalid", message: "synthetic")))
        await withScreen(controls, showing: .add) { model in
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
        await withScreen(controls, showing: .add) { model in
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
            XCTAssertNil(rendered[ProviderSettingsView.fieldIssueIdentifier(.endpoint)], "no hidden value blocks saving")
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

    /// Types into the hosted entry's buffer, the object the real `SecureField` is bound to, then presses its button.
    /// (SwiftUI does not expose the field as a `UITextField` in this host, so the binding is the reachable surface.)
    private func typeAndSubmit(_ secret: String, into identifier: String) async {
        guard let buffer = buffers[identifier] else { return XCTFail("\(identifier) is hosted") }
        buffer.text = secret
        await settle()
        buffer.press?()
        await settle()
    }

    func testTheHostedSecretFieldIsEmptyAfterStoreSucceedsAndAfterItFails() async {
        let credentials = FakeProviderSettingsCredentials()
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { _ in
            await typeAndSubmit(secret, into: ProviderSettingsView.secretFieldIdentifier)
            XCTAssertEqual(credentials.added, [Data(secret.utf8)], "the secret went to the credential service")
            XCTAssertEqual(buffers[ProviderSettingsView.secretFieldIdentifier]?.text, "")
            XCTAssertEqual(
                text(ProviderSettingsView.credentialStateIdentifier), ProviderSettingsCredentialState.stored.message)

            credentials.updateResult = .failure(CredentialError.invalidated)
            await typeAndSubmit(secret, into: ProviderSettingsView.secretFieldIdentifier)
            XCTAssertEqual(credentials.updated.count, 1)
            XCTAssertEqual(buffers[ProviderSettingsView.secretFieldIdentifier]?.text, "")
            XCTAssertEqual(
                text(ProviderSettingsView.credentialStateIdentifier),
                ProviderSettingsText.credentialFailure(CredentialError.invalidated))
            XCTAssertFalse(rendered.values.contains { $0.text.contains(secret) })
        }
    }

    private func accessibilityText(of view: UIView?) -> String {
        guard let view else { return "" }
        let own = [view.accessibilityLabel, view.accessibilityValue, view.accessibilityHint].compactMap { $0 }
        return (own + view.subviews.map { accessibilityText(of: $0) }).joined(separator: "\n")
    }

    func testTheHostedSecretFieldNeverExposesTheSecretThroughAccessibilityOrTheRenderReport() async {
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { _ in
            guard let buffer = buffers[ProviderSettingsView.secretFieldIdentifier] else { return XCTFail("hosted") }
            buffer.text = secret
            await settle()
            XCTAssertEqual(buffer.text, secret, "the field is live")
            let hosted = window?.rootViewController?.view
            XCTAssertFalse(accessibilityText(of: hosted).contains(secret), "accessibility labels and values")
            XCTAssertFalse(rendered.values.contains { $0.text.contains(secret) }, "render report")
        }
    }

    func testTheHostedSecretFieldsEmptyWhenTheAppResignsActiveAndWhenTheyLeaveTheScreen() async {
        let secret = "synthetic-secret-value"
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing)) { model in
            let fieldIdentifier = ProviderSettingsView.secretFieldIdentifier
            buffers[fieldIdentifier]?.text = secret
            await settle()
            XCTAssertEqual(buffers[fieldIdentifier]?.text, secret)
            NotificationCenter.default.post(name: UIApplication.willResignActiveNotification, object: nil)
            await settle()
            XCTAssertEqual(buffers[fieldIdentifier]?.text, "", "emptied when the app leaves the foreground")

            buffers[fieldIdentifier]?.text = secret
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

    func testBackingOutOfTheFormDeletesTheUnsavedSecretAndClearsTheDraft() async {
        let credentials = FakeProviderSettingsCredentials()
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), credentials: credentials) { model in
            model.draft.profileName = "unsaved"
            model.storeCredential(Data("synthetic-secret-value".utf8))
            XCTAssertEqual(model.draft.credentialReference, "11111111-1111-1111-1111-111111111111")
            // What the Processing screen calls when Add provider or Advanced is dismissed.
            model.abandonDraft()
            await settle()
            XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
            XCTAssertEqual(model.draft.credentialReference, "")
            XCTAssertEqual(model.draft.profileName, "")
        }
    }

    func testBackingOutOfAScreenOneLevelDownThroughTheProcessingScreenDeletesTheUnsavedSecret() async {
        for route in [\ProviderSettingsNavigation.showsAddProvider, \.showsAdvanced] {
            let credentials = FakeProviderSettingsCredentials()
            await withScreen(
                FakeProviderSettingsControls(listing: Self.mixedListing), showing: .main, credentials: credentials
            ) { model in
                navigation[keyPath: route] = true
                await settle()
                model.draft.profileName = "unsaved"
                model.storeCredential(Data("synthetic-secret-value".utf8))
                await settle()
                XCTAssertEqual(credentials.deleted, [], "the secret is kept while its screen is open")
                XCTAssertEqual(model.draft.credentialReference, "11111111-1111-1111-1111-111111111111")
                // Back on the pushed screen sets the link's binding to false.
                navigation[keyPath: route] = false
                await settle()
                XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
                XCTAssertEqual(model.draft.credentialReference, "")
                XCTAssertEqual(model.draft.profileName, "")
            }
        }
    }

    func testEveryScreenStaysInsideTheWindowAtTheLargestTextSize() async {
        textSize = .accessibility5
        defer { textSize = .large }
        for screen in [Screen.main, .add, .advanced] {
            await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: screen) { _ in
                XCTAssertFalse(rendered.isEmpty)
                for (identifier, element) in rendered where !element.frame.isEmpty {
                    XCTAssertGreaterThanOrEqual(element.frame.minX, 0, identifier)
                    XCTAssertLessThanOrEqual(element.frame.maxX, Self.phone.width, identifier)
                }
                if screen == .main {
                    let rows = ProviderProcessingChoice.allCases
                        .compactMap { rendered[ProviderSettingsView.choiceIdentifier($0)]?.frame }
                        .sorted { $0.minY < $1.minY }
                    XCTAssertEqual(rows.count, ProviderProcessingChoice.allCases.count)
                    for (upper, lower) in zip(rows, rows.dropFirst()) {
                        XCTAssertLessThanOrEqual(upper.maxY, lower.minY + 0.5, "choice rows do not overlap")
                    }
                }
            }
        }
    }

    func testTheChoiceRowsActionSetsTheDefaultOrReturnsToThisPhoneAndIgnoresTheSelectedChoice() async {
        let controls = FakeProviderSettingsControls(listing: Self.mixedListing)
        await withScreen(controls, showing: .main) { model in
            await model.select(.onThisPhone)
            XCTAssertEqual(controls.defaultsCleared, 0, "the choice already in use does nothing")
            await model.select(.ownServer)
            XCTAssertEqual(controls.defaultsSet, [], "an unverified server cannot be chosen")
            await model.select(.cloudProvider)
            XCTAssertEqual(controls.defaultsSet, ["hosted-v1"])
        }
        let hostedDefault = providerListing(
            supportLevels: [("hosted", "hosted-v1", "open_ai", false, "supported")], defaultVersion: "hosted-v1")
        let selected = FakeProviderSettingsControls(listing: hostedDefault)
        await withScreen(selected, showing: .main) { model in
            await model.select(.cloudProvider)
            XCTAssertEqual(selected.defaultsSet, [], "the selected choice does nothing")
            await model.select(.onThisPhone)
            XCTAssertEqual(selected.defaultsCleared, 1)
        }
    }

    func testAStoredServerThatCannotBeChosenYetIsNotToldToAddAProvider() async {
        await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: .main) { model in
            XCTAssertTrue(model.hasStoredVersion(for: .ownServer))
            XCTAssertNil(model.candidateVersion(for: .ownServer))
        }
        let hostedOnly = providerListing(
            supportLevels: [("hosted", "hosted-v1", "open_ai", false, "supported")], defaultVersion: nil)
        await withScreen(FakeProviderSettingsControls(listing: hostedOnly), showing: .main) { model in
            XCTAssertFalse(model.hasStoredVersion(for: .ownServer))
            XCTAssertTrue(model.hasStoredVersion(for: .cloudProvider))
        }
    }

    func testALockedSessionShowsOnlyTheLockedNotice() async {
        scope = .captureOnly
        for screen in [Screen.main, .add, .advanced] {
            await withScreen(FakeProviderSettingsControls(listing: Self.mixedListing), showing: screen) { _ in
                XCTAssertEqual(text(ProviderSettingsView.lockedIdentifier), ProviderSettingsText.lockedText)
                XCTAssertNil(rendered[ProviderSettingsView.modeIdentifier])
                XCTAssertNil(rendered[ProviderSettingsView.saveIdentifier])
                XCTAssertNil(rendered[ProviderSettingsView.makeDefaultIdentifier("hosted-v1")])
                XCTAssertNil(rendered[ProviderSettingsView.choiceIdentifier(.onThisPhone)])
                XCTAssertNil(rendered[ProviderSettingsView.addProviderIdentifier])
            }
        }
    }
}

private extension Result {
    var failureValueForView: Failure? {
        if case .failure(let failure) = self { return failure }
        return nil
    }
}
