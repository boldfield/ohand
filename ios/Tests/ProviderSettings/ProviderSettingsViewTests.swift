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

    private static let mixedListing = providerListing(
        supportLevels: [
            ("hosted", "hosted-v1", "open_ai", false, "supported"),
            ("private", "private-v1", "self_hosted", false, "unverified"),
        ], defaultVersion: nil)

    /// Shows the screen over `controls`, lets its first read land, runs `body`, then takes the screen away.
    private func withScreen(
        _ controls: FakeProviderSettingsControls, _ body: (ProviderSettingsModel) async throws -> Void
    ) async rethrows {
        rendered = [:]
        let model = ProviderSettingsModel(controls: controls, readScope: { [unowned self] in scope })
        let hosting = UIHostingController(
            rootView: NavigationStack {
                ProviderSettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
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
