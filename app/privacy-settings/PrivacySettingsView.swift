import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// The processing-policy screen: every route with what it may send, and deliberate steps to allow one more capability
/// on one route for one destination. Everything it shows comes from `PrivacySettingsModel`.
struct PrivacySettingsView: View {
    static let noticeIdentifier = "privacy-settings.notice"
    static let lockedIdentifier = "privacy-settings.locked"
    static let failureIdentifier = "privacy-settings.failure"
    static let tryAgainIdentifier = "privacy-settings.try-again"
    static let noProfilesIdentifier = "privacy-settings.no-profiles"
    static let questionIdentifier = "privacy-settings.question"
    static let confirmationNoteIdentifier = "privacy-settings.confirmation-note"
    static let allowIdentifier = "privacy-settings.allow"
    static let cancelIdentifier = "privacy-settings.cancel"
    static let actionStateIdentifier = "privacy-settings.action-state"
    static let changeQuestionIdentifier = "privacy-settings.change-question"
    static let changeConfirmIdentifier = "privacy-settings.change-confirm"
    static let queuedWorkIdentifier = "privacy-settings.queued-work"
    static let queuedWorkUnreadableIdentifier = "privacy-settings.queued-work-unreadable"
    static let requeueQuestionIdentifier = "privacy-settings.requeue-question"
    static let requeueConfirmIdentifier = "privacy-settings.requeue-confirm"
    static let requeueRecordIdentifier = "privacy-settings.requeue-record"
    static let requeueRecordUnreadableIdentifier = "privacy-settings.requeue-record-unreadable"
    static let requeueRecordRetryIdentifier = "privacy-settings.requeue-record-retry"
    static let heldWorkIdentifier = "privacy-settings.held-work"
    static let retiredWorkIdentifier = "privacy-settings.retired-work"
    static let retiredWorkUnreadableIdentifier = "privacy-settings.retired-work-unreadable"
    static let followedWorkIdentifier = "privacy-settings.followed-work"
    static let followedWorkIncompleteIdentifier = "privacy-settings.followed-work-incomplete"
    static let followedWorkCheckAgainIdentifier = "privacy-settings.followed-work-check-again"
    static func followedJobIdentifier(_ job: String) -> String { "privacy-settings.followed-job.\(job)" }
    static func queuedJobIdentifier(_ job: String) -> String { "privacy-settings.queued-job.\(job)" }
    static func retiredJobIdentifier(_ job: String) -> String { "privacy-settings.retired-job.\(job)" }
    static func retiredVersionIdentifier(_ version: String) -> String { "privacy-settings.retired.\(version)" }
    static func retiredShowMoreIdentifier(_ version: String) -> String { "privacy-settings.retired-more.\(version)" }
    static func revokeIdentifier(_ route: String, _ capability: String) -> String {
        "privacy-settings.revoke.\(route).\(capability)"
    }
    static func repinIdentifier(_ route: String) -> String { "privacy-settings.repin.\(route)" }
    static func revokeProfileIdentifier(_ version: String) -> String { "privacy-settings.revoke-profile.\(version)" }
    static func queuedVersionIdentifier(_ version: String) -> String { "privacy-settings.queued.\(version)" }
    static func requeueOfferIdentifier(_ job: String, _ version: String) -> String {
        "privacy-settings.requeue.\(job).\(version)"
    }
    static func routeIdentifier(_ route: String) -> String { "privacy-settings.route.\(route)" }
    static func routeStatusIdentifier(_ route: String) -> String { "privacy-settings.route-status.\(route)" }
    static func offerIdentifier(_ route: String, _ capability: String, _ version: String) -> String {
        "privacy-settings.offer.\(route).\(capability).\(version)"
    }

    /// What each identified part of the screen shows, collected so a hosted test can read the rendered words.
    struct RenderedKey: PreferenceKey {
        static var defaultValue: [String: String] { [:] }
        static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
            value.merge(nextValue()) { _, new in new }
        }
    }

    @ObservedObject var model: PrivacySettingsModel
    /// Lets a hosted test see what the screen renders; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    init(model: PrivacySettingsModel, renderReporter: (([String: String]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

    var body: some View {
        SettingsScreenChrome(title: "Privacy", infoTitle: "Privacy", infoParagraphs: PrivacySettingsText.infoParagraphs) {
            content
        }
        .task { await model.refresh() }
        .onDisappear { model.clear() }
        .onPreferenceChange(RenderedKey.self) { renderReporter?($0) }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView("Loading").frame(maxWidth: .infinity)
        case .locked:
            note(PrivacySettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            DSPillButton(title: "Try again", style: .secondary) { Task { await model.refresh() } }
                .accessibilityIdentifier(Self.tryAgainIdentifier)
                .reportPrivacyRendered(Self.tryAgainIdentifier, text: "Try again")
        case .loaded:
            ForEach(model.routes, id: \.routeID) { route in routeSection(route) }
            // One footer line: with no provider saved, it says where to add one.
            if model.listing?.profiles.isEmpty ?? true {
                note(PrivacySettingsText.noProfilesNotice, identifier: Self.noProfilesIdentifier)
            } else {
                note(PrivacySettingsText.localFirstNotice, identifier: Self.noticeIdentifier)
            }
            queuedWorkSection
            if let proposal = model.proposal { proposalCard(proposal) }
            if let change = model.change { changeCard(change) }
            if let requeue = model.requeueProposal { requeueCard(requeue) }
            if let record = model.lastRequeue {
                note(PrivacySettingsText.requeueRecord(record), identifier: Self.requeueRecordIdentifier)
            }
            if model.requeueRecordUnreadable {
                note(PrivacySettingsText.requeueRecordUnreadableNotice, identifier: Self.requeueRecordUnreadableIdentifier)
                DSPillButton(title: PrivacySettingsText.requeueRecordRetry, style: .secondary) {
                    Task { await model.loadRequeueRecord() }
                }
                .reportPrivacyRendered(Self.requeueRecordRetryIdentifier, text: PrivacySettingsText.requeueRecordRetry)
                .accessibilityIdentifier(Self.requeueRecordRetryIdentifier)
            }
            if let state = model.actionState {
                note(state.message, identifier: Self.actionStateIdentifier)
            }
        }
    }

    /// One decision per route: what it may send now, then a row for each destination it could also be allowed.
    private func routeSection(_ route: RoutePolicyRoute) -> some View {
        DSGroupedSection(title: PrivacySettingsText.routeLabel(route)) {
            if route.grantedCapabilities.isEmpty {
                DSRow(label: PrivacySettingsText.routeLocalOnly, iconSystemName: "iphone", showsChevron: false)
                    .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
                    .reportPrivacyRendered(Self.routeStatusIdentifier(route.routeID), text: PrivacySettingsText.routeLocalOnly)
            } else {
                VStack(spacing: 0) {
                    ForEach(route.grantedCapabilities, id: \.capability) { grant in
                        DSRow(
                            label: PrivacySettingsText.grantSummary(grant), iconSystemName: "checkmark.shield",
                            showsChevron: false)
                        revokeRow(route: route, capability: grant.capability)
                    }
                }
                .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
                .reportPrivacyRendered(
                    Self.routeStatusIdentifier(route.routeID),
                    text: route.grantedCapabilities.map(PrivacySettingsText.grantSummary).joined(separator: "\n"))
            }
            if model.isGranted(routeID: route.routeID, capability: "text_interpretation") {
                repinRow(route: route)
            }
            ForEach(PrivacySettingsModel.grantableCapabilities, id: \.self) { capability in
                ForEach(model.versionsToOffer(routeID: route.routeID, capability: capability), id: \.version.profileVersion) { offer in
                    offerRow(route: route, capability: capability, offer: offer)
                }
            }
        }
        .accessibilityIdentifier(Self.routeIdentifier(route.routeID))
        .reportPrivacyRendered(Self.routeIdentifier(route.routeID), text: PrivacySettingsText.routeLabel(route))
    }

    private func revokeRow(route: RoutePolicyRoute, capability: String) -> some View {
        let label = "Stop \(PrivacySettingsText.capabilityName(capability).lowercased())"
        let identifier = Self.revokeIdentifier(route.routeID, capability)
        return DSRow(label: label, iconSystemName: "xmark.shield", showsChevron: false) {
            model.proposeRevoke(routeID: route.routeID, capability: capability)
        }
        .reportPrivacyRendered(identifier, text: label)
        .accessibilityIdentifier(identifier)
        .disabled(model.isWorking)
    }

    private func repinRow(route: RoutePolicyRoute) -> some View {
        let label = "Queue waiting notes with the provider selected now"
        let identifier = Self.repinIdentifier(route.routeID)
        return DSRow(label: label, iconSystemName: "arrow.triangle.2.circlepath", showsChevron: false) {
            model.proposeRepinWaitingWork(routeID: route.routeID)
        }
        .reportPrivacyRendered(identifier, text: label)
        .accessibilityIdentifier(identifier)
        .disabled(model.isWorking)
    }

    /// Per unrevoked stored version: how much work waits behind it, the option to revoke it, and a deliberate move for
    /// each listed job, and each job a revocation retired and the core has not seen moved, to each other unrevoked
    /// version.
    @ViewBuilder
    private var queuedWorkSection: some View {
        if model.queuedWorkUnreadable {
            note(PrivacySettingsText.queuedWorkUnreadableNotice, identifier: Self.queuedWorkUnreadableIdentifier)
        }
        if !model.queuedWork.isEmpty {
            DSGroupedSection(title: PrivacySettingsText.queuedWorkTitle) {
                ForEach(model.queuedWork, id: \.version.profileVersion) { queued in
                    queuedVersionRows(queued)
                }
            }
            .accessibilityIdentifier(Self.queuedWorkIdentifier)
        }
        if !model.queuedWork.isEmpty, !model.routesHoldingWork.isEmpty {
            note(PrivacySettingsText.heldWork(model.routesHoldingWork), identifier: Self.heldWorkIdentifier)
        }
        if model.retiredWorkUnreadable {
            note(PrivacySettingsText.retiredWorkUnreadableNotice, identifier: Self.retiredWorkUnreadableIdentifier)
        }
        if !model.retiredWork.isEmpty {
            DSGroupedSection(title: PrivacySettingsText.retiredWorkTitle) {
                ForEach(model.retiredWork, id: \.profileVersion) { retired in retiredVersionRows(retired) }
            }
            .accessibilityIdentifier(Self.retiredWorkIdentifier)
        }
        followedWorkSection
        if let listing = model.listing {
            let revocable = listing.profiles.flatMap { profile in
                profile.versions.filter { !$0.revoked }.map { (profile.profileID, $0) }
            }
            if !revocable.isEmpty {
                DSGroupedSection(title: "Provider versions") {
                    ForEach(revocable, id: \.1.profileVersion) { profileID, version in
                        let label = "Revoke \(PrivacySettingsText.versionLabel(profileID: profileID, version: version))"
                        let identifier = Self.revokeProfileIdentifier(version.profileVersion)
                        DSRow(label: label, iconSystemName: "trash", showsChevron: false) {
                            model.proposeRevokeProfile(profileVersion: version.profileVersion)
                        }
                        .reportPrivacyRendered(identifier, text: label)
                        .accessibilityIdentifier(identifier)
                        .disabled(model.isWorking)
                    }
                }
            }
        }
    }

    /// What became of the work pending at the last route revocation, as the core reports it now, with a way to read it
    /// again while a request already sent is still waiting for its reply.
    @ViewBuilder
    private var followedWorkSection: some View {
        if !model.followedWork.isEmpty {
            DSGroupedSection(title: PrivacySettingsText.followedWorkTitle) {
                ForEach(model.followedWork, id: \.jobID) { job in
                    let text = PrivacySettingsText.followedJob(job)
                    let identifier = Self.followedJobIdentifier(job.jobID)
                    DSRow(label: text, iconSystemName: "arrow.uturn.backward", showsChevron: false)
                        .reportPrivacyRendered(identifier, text: text)
                        .accessibilityIdentifier(identifier)
                }
                DSRow(label: PrivacySettingsText.followedWorkCheckAgain, iconSystemName: "arrow.clockwise", showsChevron: false) {
                    Task { await model.refresh() }
                }
                .reportPrivacyRendered(Self.followedWorkCheckAgainIdentifier, text: PrivacySettingsText.followedWorkCheckAgain)
                .accessibilityIdentifier(Self.followedWorkCheckAgainIdentifier)
                .disabled(model.isWorking)
            }
            .accessibilityIdentifier(Self.followedWorkIdentifier)
        }
        if model.followedWorkIncomplete {
            note(PrivacySettingsText.followedWorkIncompleteNotice, identifier: Self.followedWorkIncompleteIdentifier)
        }
    }

    /// The core's durable list of what one version's revocation retired: a heading with the full count, the pages read
    /// so far, a move offer for each job the core has not seen moved, and a row that reads the next page.
    @ViewBuilder
    private func retiredVersionRows(_ retired: PrivacyRetiredVersion) -> some View {
        let heading = retired.versionLabel + " · " + PrivacySettingsText.retiredCount(retired)
        let identifier = Self.retiredVersionIdentifier(retired.profileVersion)
        DSRow(label: heading, iconSystemName: "xmark.bin", showsChevron: false)
            .reportPrivacyRendered(identifier, text: heading)
            .accessibilityIdentifier(identifier)
        ForEach(retired.jobs, id: \.jobID) { job in
            let text = PrivacySettingsText.retiredJob(job)
            DSRow(label: text, iconSystemName: job.isMovable ? "clock.badge.xmark" : "arrow.right.circle", showsChevron: false)
                .reportPrivacyRendered(Self.retiredJobIdentifier(job.jobID), text: text)
                .accessibilityIdentifier(Self.retiredJobIdentifier(job.jobID))
            if job.isMovable { moveOffers(jobID: job.jobID, from: retired.profileVersion) }
        }
        if retired.nextCursor != nil {
            let more = PrivacySettingsText.retiredShowMore(retired)
            let moreIdentifier = Self.retiredShowMoreIdentifier(retired.profileVersion)
            DSRow(label: more, iconSystemName: "ellipsis", showsChevron: false) {
                Task { await model.loadMoreRetiredWork(profileVersion: retired.profileVersion) }
            }
            .reportPrivacyRendered(moreIdentifier, text: more)
            .accessibilityIdentifier(moreIdentifier)
            .disabled(model.isWorking || model.isLoadingRetiredWork(profileVersion: retired.profileVersion))
        }
    }

    @ViewBuilder
    private func queuedVersionRows(_ queued: PrivacyQueuedVersion) -> some View {
        let heading = PrivacySettingsText.versionLabel(profileID: queued.profileID, version: queued.version)
            + " · " + PrivacySettingsText.queuedCount(queued.jobs)
        let identifier = Self.queuedVersionIdentifier(queued.version.profileVersion)
        DSRow(label: heading, iconSystemName: "tray.full", showsChevron: false)
            .reportPrivacyRendered(identifier, text: heading)
            .accessibilityIdentifier(identifier)
        ForEach(queued.jobs.jobs, id: \.jobID) { job in
            let jobText = PrivacySettingsText.queuedJob(job)
            DSRow(label: jobText, iconSystemName: "clock", showsChevron: false)
                .reportPrivacyRendered(Self.queuedJobIdentifier(job.jobID), text: jobText)
                .accessibilityIdentifier(Self.queuedJobIdentifier(job.jobID))
            moveOffers(jobID: job.jobID, from: queued.version.profileVersion)
        }
    }

    @ViewBuilder
    private func moveOffers(jobID: String, from profileVersion: String) -> some View {
        ForEach(otherVersions(than: profileVersion), id: \.version.profileVersion) { target in
            let label = "Move to \(PrivacySettingsText.versionLabel(profileID: target.profileID, version: target.version))"
            let offerIdentifier = Self.requeueOfferIdentifier(jobID, target.version.profileVersion)
            DSRow(label: label, iconSystemName: "arrow.right", showsChevron: false) {
                model.proposeRequeue(jobID: jobID, toProfileVersion: target.version.profileVersion)
            }
            .reportPrivacyRendered(offerIdentifier, text: label)
            .accessibilityIdentifier(offerIdentifier)
            .disabled(model.isWorking)
        }
    }

    private func otherVersions(than profileVersion: String) -> [PrivacyOfferedVersion] {
        guard let listing = model.listing else { return [] }
        return listing.profiles.flatMap { profile in
            profile.versions.filter { !$0.revoked && $0.profileVersion != profileVersion }
                .map { PrivacyOfferedVersion(profileID: profile.profileID, version: $0) }
        }
    }

    private func changeCard(_ change: PrivacyChangeProposal) -> some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                Text(PrivacySettingsText.changeQuestion(change))
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.changeQuestionIdentifier)
                    .reportPrivacyRendered(Self.changeQuestionIdentifier, text: PrivacySettingsText.changeQuestion(change))
                DSPillButton(title: "Confirm", style: .primary) { Task { await model.confirmChange(change) } }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.changeConfirmIdentifier, text: "Confirm")
                    .accessibilityIdentifier(Self.changeConfirmIdentifier)
                    .disabled(model.isWorking)
                DSPillButton(title: "Not now", style: .plain) { model.cancel() }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.cancelIdentifier, text: "Not now")
                    .accessibilityIdentifier(Self.cancelIdentifier)
                    .disabled(model.isWorking)
            }
        }
    }

    private func requeueCard(_ proposal: PrivacyRequeueProposal) -> some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                Text(PrivacySettingsText.requeueQuestion(proposal))
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.requeueQuestionIdentifier)
                    .reportPrivacyRendered(Self.requeueQuestionIdentifier, text: PrivacySettingsText.requeueQuestion(proposal))
                DSPillButton(title: "Move", style: .primary) { Task { await model.confirmRequeue(proposal) } }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.requeueConfirmIdentifier, text: "Move")
                    .accessibilityIdentifier(Self.requeueConfirmIdentifier)
                    .disabled(model.isWorking)
                DSPillButton(title: "Not now", style: .plain) { model.cancel() }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.cancelIdentifier, text: "Not now")
                    .accessibilityIdentifier(Self.cancelIdentifier)
                    .disabled(model.isWorking)
            }
        }
    }

    private func offerRow(
        route: RoutePolicyRoute, capability: String, offer: PrivacyOfferedVersion
    ) -> some View {
        let label = "Allow \(PrivacySettingsText.capabilityName(capability).lowercased()) via "
            + PrivacySettingsText.versionLabel(profileID: offer.profileID, version: offer.version)
        let identifier = Self.offerIdentifier(route.routeID, capability, offer.version.profileVersion)
        return DSRow(label: label, iconSystemName: "plus", showsChevron: false) {
            model.propose(routeID: route.routeID, capability: capability, profileVersion: offer.version.profileVersion)
        }
        .reportPrivacyRendered(identifier, text: label)
        .accessibilityIdentifier(identifier)
        // Disabled outside the report so the report reads the disabled environment.
        .disabled(model.isWorking)
    }

    /// The deliberate step: the question, its one footer line, then Allow and Not now.
    private func proposalCard(_ proposal: PrivacyGrantProposal) -> some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                Text(PrivacySettingsText.proposalQuestion(proposal))
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.questionIdentifier)
                    .reportPrivacyRendered(Self.questionIdentifier, text: PrivacySettingsText.proposalQuestion(proposal))
                Text(PrivacySettingsText.confirmationFooter)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.confirmationNoteIdentifier)
                    .reportPrivacyRendered(Self.confirmationNoteIdentifier, text: PrivacySettingsText.confirmationFooter)
                DSPillButton(title: "Allow", style: .primary) { Task { await model.confirm(proposal) } }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.allowIdentifier, text: "Allow")
                    .accessibilityIdentifier(Self.allowIdentifier)
                    .disabled(model.isWorking)
                DSPillButton(title: "Not now", style: .plain) { model.cancel() }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.cancelIdentifier, text: "Not now")
                    .accessibilityIdentifier(Self.cancelIdentifier)
                    .disabled(model.isWorking)
            }
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, DSSpacing.md)
            .accessibilityIdentifier(identifier)
            .reportPrivacyRendered(identifier, text: text)
    }
}

/// Reports what an identified part shows, read from the laid-out view: nothing while it has no size, and
/// " (disabled)" after the text when the environment has disabled it.
private struct PrivacyRenderedReport: ViewModifier {
    let identifier: String
    let text: String
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.background(
            GeometryReader { proxy in
                Color.clear.preference(
                    key: PrivacySettingsView.RenderedKey.self,
                    value: proxy.size.width > 0 && proxy.size.height > 0
                        ? [identifier: isEnabled ? text : "\(text) (disabled)"] : [:])
            })
    }
}

extension View {
    fileprivate func reportPrivacyRendered(_ identifier: String, text: String) -> some View {
        modifier(PrivacyRenderedReport(identifier: identifier, text: text))
    }
}

extension PrivacySettingsModel {
    /// The model as the app assembles it: the root's provider and job configuration and its gated item status read,
    /// gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> PrivacySettingsModel {
        PrivacySettingsModel(
            controls: PrivacySettingsServices(
                providers: root.providerConfiguration, jobs: root.jobConfiguration,
                itemStatus: { [weak root] itemID, completion in
                    guard let root else {
                        completion(.failure(CoreFailure(
                            errorClass: .unauthorized, code: "read_not_authenticated", message: "the app has closed")))
                        return
                    }
                    root.itemStatus(itemID: itemID, completion: completion)
                }),
            readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
