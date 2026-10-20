import Foundation
import OhAndCoreBridge
import OhAndServices

/// The words the processing-policy screen uses. Kept apart from the model so each statement is checked against the
/// case that produces it.
enum PrivacySettingsText {
    static let lockedText = "Your saved items are locked. Go back and open Settings again to unlock them."
    /// The screen's one footer line under the routes.
    static let localFirstNotice = "Nothing leaves the device unless you allow it here."
    /// The full explanation behind the info button.
    static let localFirstExplanation =
        "Capturing, saving and searching work on this device without any setup. Nothing is sent anywhere unless you "
        + "allow it on this screen."
    static let stickyNotice =
        "Each route keeps the scope and permissions you gave it. Nothing the app reads or a provider answers can "
        + "change them; only you can, here."
    static let confirmationNote =
        "Allowing this lets notes saved on this route be sent to the destination named above for this one purpose "
        + "only. It does not allow any other purpose, destination or route, and it changes nothing already saved."
    /// The question card's one footer line; `confirmationNote` is its full explanation in the info sheet.
    static let confirmationFooter = "Only this purpose, destination and route."
    static let infoParagraphs = [localFirstExplanation, stickyNotice, confirmationNote]
    static let noProfilesNotice = "No provider yet. Add one in Processing; until then everything stays here."
    static let routeLocalOnly = "Nothing leaves the device"
    static let cancelledNotice = "Nothing was changed. Everything still works on this device."

    static func scopeName(_ scope: String) -> String {
        switch scope {
        case "personal": return "Personal"
        case "work": return "Work"
        default: return "Other"
        }
    }

    static func routeLabel(_ route: RoutePolicyRoute) -> String {
        "\(scopeName(route.scope)) route · \(route.routeName)"
    }

    static func capabilityName(_ capability: String) -> String {
        switch capability {
        case "text_interpretation": return "Sorting your text notes"
        case "review": return "Quality review"
        default: return "Another purpose"
        }
    }

    static func destinationName(_ destinationClass: String) -> String {
        switch destinationClass {
        case "cloud": return "a hosted provider"
        case "private_server": return "your private server"
        default: return "an unrecognised destination"
        }
    }

    static func protocolName(_ providerProtocol: String) -> String {
        switch providerProtocol {
        case "open_ai": return "OpenAI"
        case "anthropic": return "Anthropic"
        case "self_hosted": return "Private server"
        default: return "Provider"
        }
    }

    /// The destination a stored profile version stands for, as the user would recognise it.
    static func versionLabel(profileID: String, version: ProviderProfileListAnswer.Version) -> String {
        "\(profileID) · \(protocolName(version.providerProtocol)) · \(version.model)"
    }

    static func grantSummary(_ grant: RoutePolicyGrant) -> String {
        let destinations = grant.destinationClasses.map(destinationName).joined(separator: " and ")
        let suffix = destinations.isEmpty ? "" : " via \(destinations)"
        return "Allowed: \(capabilityName(grant.capability))\(suffix)"
    }

    static func proposalQuestion(_ proposal: PrivacyGrantProposal) -> String {
        "Allow \(capabilityName(proposal.capability).lowercased()) on \(proposal.routeLabel) to use "
            + "\(proposal.versionLabel)?"
    }

    static func result(_ outcome: RoutePolicyChangeOutcome.Disposition, _ proposal: PrivacyGrantProposal) -> String {
        switch outcome {
        case .granted:
            return "Allowed \(capabilityName(proposal.capability).lowercased()) on \(proposal.routeLabel). "
                + "Nothing else was allowed."
        case .unchanged: return "That was already allowed. Nothing changed."
        case .revoked: return "Nothing changed."
        }
    }

    static func failure(_ error: Error) -> String {
        guard let error = error as? ProviderConfigurationError else { return "The permission could not be changed." }
        switch error {
        case .notAuthenticated: return lockedText
        case .invalidRequest: return "That choice is not valid. Nothing was changed."
        case .unreadable: return "The result was unclear. Check the route below before trying again."
        case .core(let failure):
            switch failure.code {
            case "profile_unavailable": return "That profile version is no longer available. Nothing was changed."
            case "profile_revoked": return "That profile version was revoked. Nothing was changed."
            case "route_unknown": return "That route no longer exists. Nothing was changed."
            case "capability_not_grantable": return "That purpose runs on this device and needs no permission."
            default: return "The permission could not be changed. Nothing was changed."
            }
        }
    }

    static func loadFailure(_ error: Error) -> String {
        if let error = error as? ProviderConfigurationError, case .notAuthenticated = error { return lockedText }
        return "Permissions could not be read. Capture and search still work; try again later."
    }

    static let queuedWorkTitle = "Queued work"
    static let queuedWorkUnreadableNotice = "Queued work could not be read. Permissions above are still accurate."
    static func revokeQuestion(routeLabel: String, capability: String) -> String {
        "Stop \(capabilityName(capability).lowercased()) on \(routeLabel) from being sent anywhere?"
    }

    static func revokeProfileQuestion(_ versionLabel: String) -> String {
        "Revoke \(versionLabel)? Work queued for it is retired, and it cannot be allowed again."
    }

    static func repinQuestion(routeLabel: String) -> String {
        "Queue waiting notes on \(routeLabel) with the provider selected now? Only a provider you already allowed for "
            + "this route is used."
    }

    static func changeQuestion(_ change: PrivacyChangeProposal) -> String {
        switch change.target {
        case .revokeRouteCapability(_, let routeLabel, let capability):
            return revokeQuestion(routeLabel: routeLabel, capability: capability)
        case .revokeProfileVersion(_, let versionLabel): return revokeProfileQuestion(versionLabel)
        case .repinWaitingWork(_, let routeLabel): return repinQuestion(routeLabel: routeLabel)
        }
    }

    /// The result of taking a route permission away. It states the core's guarantee, not a per-job outcome: which
    /// pending job the change affects is not known here, so each one's actual state is listed under `followedWorkTitle`.
    static func revokeResult(
        _ outcome: RoutePolicyChangeOutcome.Disposition, routeLabel: String, capability: String,
        stillAllowed: Bool? = nil, followed: Int = 0
    ) -> String {
        switch outcome {
        case .revoked:
            let stopped = "Stopped \(capabilityName(capability).lowercased()) on \(routeLabel)."
            if stillAllowed == true { return stopped + " The stored policy still lists it as allowed; check the route above." }
            let guarantee = stopped + " Work not yet sent is checked against the permissions now in force before it "
                + "leaves the device. A request already sent cannot be recalled; a reply to it changes a note only if "
                + "the permissions in force when it arrives allow it."
            guard followed > 0 else { return guarantee }
            return guarantee + " What became of the \(followed) job\(followed == 1 ? "" : "s") pending at this change is "
                + "shown under \"\(followedWorkTitle)\"."
        case .unchanged: return "That was not allowed, so nothing changed."
        case .granted: return "Nothing changed."
        }
    }

    static let followedWorkTitle = "Pending at your last change"
    static let followedWorkIncompleteNotice =
        "Some work pending at that change could not be listed here. The core checks it the same way."
    static let followedWorkCheckAgain = "Check again"

    /// One job pending when a route permission was taken away. The job's own state comes from the core's listing;
    /// the note's text sorting follows as a separate fact about the note, because it may describe other work on it.
    static func followedJob(_ job: PrivacyFollowedJob) -> String {
        var text = "Job \(job.jobID) · " + followedListing(job)
        if let note = job.note { text += " " + noteSorting(note) }
        return text
    }

    private static func followedListing(_ job: PrivacyFollowedJob) -> String {
        switch job.listing {
        case .listed("running"):
            return "request already sent, waiting for its reply. A reply changes the note only if the permissions in "
                + "force when it arrives allow it."
        case .listed(let status) where job.requestSent:
            return "request already sent; listed \(status) again. It is sent again, and a reply changes the note, only "
                + "if the permissions in force then allow it."
        case .listed(let status):
            return "listed \(status). It is sent only if the permissions in force then allow it."
        case .notListed where job.requestSent:
            return "no longer queued or running; its request had been sent. What this job itself returned is not "
                + "shown here; a reply changes the note only if the permissions in force when it arrived allowed it."
        case .notListed:
            return "no longer queued or running. What this job itself returned is not shown here."
        case .unknown where job.requestSent:
            return "request already sent; its current listing could not be read. Check again."
        case .unknown:
            return "its current listing could not be read. Check again."
        }
    }

    /// The note's text sorting, worded as the note's, never as the outcome of the followed job.
    private static func noteSorting(_ note: PrivacyFollowedJob.Note) -> String {
        let prefix = "The note's text sorting (which may be other work on it): "
        guard case .reported(let processing) = note else { return prefix + "could not be read." }
        switch (processing.processingState, processing.processingJobStatus) {
        case ("processed", _): return prefix + "sorted."
        case ("abstained", _): return prefix + "answered with no change."
        case ("uninterpreted", _): return prefix + "ended without a usable result."
        case (_, "awaiting_configuration"):
            return prefix + "refused under the current permissions; the note waits on this device until a permission "
                + "allows it."
        case (_, "retrying_after_transient"): return prefix + "waiting to retry."
        case ("processing", _): return prefix + "in progress."
        default: return prefix + "not sorted (\(processing.processingState))."
        }
    }

    static func profileRevoked(_ versionLabel: String, retired: Int, alreadySent: Int, stillPending: Int) -> String {
        var message = "Revoked \(versionLabel). \(retired) job\(retired == 1 ? "" : "s") retired and not moved to another provider."
        if alreadySent > 0 {
            message += " \(alreadySent) had already been sent; a reply to it does not change the note."
        }
        if stillPending > 0 { message += " The core still lists \(stillPending) as pending; check Queued work." }
        if retired > 0 { message += " Each can be moved on purpose below." }
        return message
    }

    static func profileRevokeNotStarted(_ versionLabel: String) -> String {
        "The work queued for \(versionLabel) could not be read, so it was not revoked. Nothing was changed; try again."
    }

    static func profileRevokedUnread(_ versionLabel: String) -> String {
        "Revoked \(versionLabel). The work that was waiting for it could not be read back; its jobs are retired by "
            + "the core and not moved."
    }

    static func repinResult(moved: Int, routeLabel: String, moreMayWait: Bool = false) -> String {
        if moved == 0 {
            return "Nothing was moved: no notes were waiting, or no allowed provider is selected for \(routeLabel)."
        }
        let notes = moved == 1 ? "1 waiting note" : "\(moved) waiting notes"
        let rest = moreMayWait ? " More may still be waiting; run it again to queue them." : ""
        return "Queued \(notes) on \(routeLabel) with the provider selected now." + rest
    }

    static let heldWorkNotice =
        "Queued work is listed by provider version, not by route. A route that does not allow sorting text notes "
        + "holds its work instead of sending it: "
    static let retiredWorkTitle = "Retired work"
    static func heldWork(_ routeLabels: [String]) -> String { heldWorkNotice + routeLabels.joined(separator: ", ") + "." }
    static func retiredJob(_ job: JobConfigurationPinnedJob) -> String { "Job \(job.jobID) · retired (provider revoked)" }
    static let requeueRecordUnreadableNotice = "The record of that move could not be read. The move itself was accepted."
    static let requeueRecordRetry = "Read the record again"

    static func queuedCount(_ jobs: JobConfigurationPinnedJobs) -> String {
        let noun = jobs.total == 1 ? "job" : "jobs"
        return jobs.truncated ? "\(jobs.total) \(noun) (showing \(jobs.jobs.count))" : "\(jobs.total) \(noun)"
    }

    static func queuedJob(_ job: JobConfigurationPinnedJob) -> String {
        let state = job.status == "running" ? "running (request already sent)" : job.status
        return "Job \(job.jobID) · \(state)"
    }

    static func requeueQuestion(_ proposal: PrivacyRequeueProposal) -> String {
        "Move job \(proposal.jobID) from \(proposal.fromVersionLabel) to \(proposal.toVersionLabel)?"
    }

    static func requeued(to versionLabel: String, recorded: Bool) -> String {
        "Moved to \(versionLabel). The old job was retired"
            + (recorded ? " and the link is recorded below." : "; its link could not be read yet.")
    }

    static func requeueRecord(_ record: JobConfigurationRequeueRecord) -> String {
        let source = record.fromProfileVersion ?? "waiting work"
        return "Moved job \(record.oldJobID) to \(record.newJobID): \(source) → \(record.toProfileVersion) "
            + "at \(record.requeuedAt)."
    }

    static func changeFailure(_ error: Error) -> String {
        if let error = error as? JobConfigurationError {
            switch error {
            case .notAuthenticated: return lockedText
            case .invalidRequest: return "That choice is not valid. Nothing was changed."
            case .notFound: return "That job or profile version no longer exists. Nothing was changed."
            case .refused: return "The change was refused, for example because the target is revoked or not allowed. Nothing was changed."
            case .core, .unreadable: return "The result was unclear. Check the lists below before trying again."
            }
        }
        return failure(error)
    }
}
