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
}
